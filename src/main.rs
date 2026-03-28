#![no_std]
#![no_main]

// Bootloader
use rp2040_boot2;
#[link_section = ".boot2"]
#[used]
pub static BOOT_LOADER: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;

const ADDRESS_MODE: u32 = 0xC0000; // 18 (BDIR) & 19 (BC1) high
const WRITE_MODE: u32 = 0x40000; // 18 (BDIR) high, 19 (BC1) low

// Deps
use defmt_rtt as _;
use panic_halt as _;

use rp2040_hal::{self as hal, Spi, gpio::{AnyPin, FunctionSpi, PinGroup, ReadPinHList, WritePinHList}};
use rp2040_hal::fugit::RateExtU32;
use embedded_hal::{digital::{OutputPin, PinState::{self, *}}};

use mipidsi::{Builder, interface::SpiInterface};
use mipidsi::{models::ILI9341Rgb565, options::ColorOrder};           // Provides the builder for Display
use embedded_graphics::{prelude::WebColors, pixelcolor::Rgb565};

use hal::{
    clocks::{init_clocks_and_plls},
    Clock,
    pac,
    sio::Sio,
    watchdog::Watchdog,
};

use usb_device::{class_prelude::*, prelude::*};

use usbd_midi::{
    message::{Message, Channel, Note},
    UsbMidiClass,
    UsbMidiPacketReader,
};

use frunk::{HCons};

// YM2149 driver
use ym2149_core::{command::{Command, CommandOutput}, chip::YM2149};

// MIDI command interpreter
mod interpreter;

#[repr(u8)]
pub enum Mode {
    /// DA7~DA0 has high impedance.
    INACTIVE,
    /// DA7~DA0 set to output mode, and contents of register currently being addressed are output.
    ///
    /// ---
    /// ### Warning!
    ///
    /// Mode::READ makes the chip output 5V to the data bus. It is **STRONGLY** recommended
    /// to use a level shifter in order to prevent permanent damage to your board.
    READ,
    /// DA7~DA0 set to input mode, and data is written to register currently being addressed.
    WRITE,
    /// DA7~DA0 set to input mode, and address is fetched from register array.
    ADDRESS,
}

impl Mode {
    pub const STATES: [(PinState, PinState, PinState); 4] = [
        (Low, High, Low),   // INACTIVE
        (Low, High, High),  // READ
        (High, High, Low),  // WRITE
        (High, High, High), // ADDRESS
    ];

    /// Returns an appropriate array of `PinState`s.
    fn pin_states(self) -> (PinState, PinState, PinState) {
        Self::STATES[self as usize]
    }
}

pub struct DualYMDataBus<H, T>
where
    HCons<H, T>: ReadPinHList + WritePinHList,
    H: AnyPin,
{
    data_bus: PinGroup<HCons<H, T>>,
    b_active: bool
}



impl<H, T> DualYMDataBus<H, T>
where
    HCons<H, T>: ReadPinHList + WritePinHList,
    H: AnyPin,
{
    pub fn new(data_bus: PinGroup<HCons<H, T>>) -> Self {
        Self {
            data_bus,
            b_active: false
        }
    }

    /// Write to the data bus along with bus control
    // Hardcoded pins!
    fn write_command(&mut self, command: Command) {
        let (mode_shift, true_register) = if command.register > 0xF {
            (2, command.register - 0xF)
        } else {
            (0, command.register)
        };

        // write address & set inactive
        self.data_bus.set_u32(
            (ADDRESS_MODE << mode_shift) + ((true_register as u32) << 2) // Address mode on correct chip & write register on pins 2-9.
        );

        self.data_bus.set_u32(0);

        // write value & set inactive
        self.data_bus.set_u32(
            (WRITE_MODE << mode_shift) + ((command.value as u32) << 2) // Write mode on correct chip & write value on pins 2-9.
        );

        self.data_bus.set_u32(0);
    }
}

impl<H, T> CommandOutput for DualYMDataBus<H, T>
where
    HCons<H, T>: ReadPinHList + WritePinHList,
    H: AnyPin,
{
    fn execute(&mut self, command: Command) {
        self.write_command(command);
    }
}

#[hal::entry]
fn main() -> ! {
    // init stuff
    let mut pac = pac::Peripherals::take().unwrap();
    let core = pac::CorePeripherals::take().unwrap();
    let mut watchdog = Watchdog::new(pac.WATCHDOG);
    let sio = Sio::new(pac.SIO);

    // External high-speed crystal on the pico board is 12Mhz
    let external_xtal_freq_hz = 12_000_000u32;
    let clocks = init_clocks_and_plls(
        external_xtal_freq_hz,
        pac.XOSC,
        pac.CLOCKS,
        pac.PLL_SYS,
        pac.PLL_USB,
        &mut pac.RESETS,
        &mut watchdog,
    )
    .ok()
    .unwrap();

    let mut timer = hal::Timer::new(pac.TIMER, &mut pac.RESETS, &clocks);
    let mut delay = DelayCompat(cortex_m::delay::Delay::new(
        core.SYST,
        clocks.system_clock.freq().to_Hz(),
    ));

    let pins = hal::gpio::Pins::new(
        pac.IO_BANK0,
        pac.PADS_BANK0,
        sio.gpio_bank0,
        &mut pac.RESETS,
    );

    // actual code

    let mut status_led = pins.gpio25.into_push_pull_output();

    let usb_bus = UsbBusAllocator::new(hal::usb::UsbBus::new(
        pac.USBCTRL_REGS,
        pac.USBCTRL_DPRAM,
        clocks.usb_clock,
        true,
        &mut pac.RESETS
    ));

    let mut midi = UsbMidiClass::new(&usb_bus, 1, 0).unwrap();

    let mut device = UsbDeviceBuilder::new(&usb_bus, UsbVidPid(0x2E8A, 0x000A))
        .device_class(0)
        .device_sub_class(0)
        .strings(&[StringDescriptors::default()
            .manufacturer("vw.dvw")
            .product("Dual YM2149 USB-MIDI Synthesizer")
            .serial_number("DZ-0001")])
        .unwrap()
        .build();

    // Frequency (in Hz, u32) of the clock the chip is connected to (Pin 22 on the YM2149)
    let master_clock_freq: u32 = 3_579_545;

    status_led.set_high();

    // Initialize a PinGroup
    let ym_pins = PinGroup::new()
        .add_pin(pins.gpio2.into_push_pull_output()) // DATA BUS
        .add_pin(pins.gpio3.into_push_pull_output())
        .add_pin(pins.gpio4.into_push_pull_output())
        .add_pin(pins.gpio5.into_push_pull_output())
        .add_pin(pins.gpio6.into_push_pull_output())
        .add_pin(pins.gpio7.into_push_pull_output())
        .add_pin(pins.gpio8.into_push_pull_output())
        .add_pin(pins.gpio9.into_push_pull_output())
        .add_pin(pins.gpio18.into_push_pull_output()) // BDIR A
        .add_pin(pins.gpio19.into_push_pull_output()) // BC1 A
        .add_pin(pins.gpio20.into_push_pull_output()) // BDIR B
        .add_pin(pins.gpio21.into_push_pull_output()); // BC1 B

    let data_bus = DualYMDataBus::new(
        ym_pins,
    );

    // Build the chip by passing:
    let mut dual_ym = YM2149::new(
        data_bus,
        master_clock_freq
    ).expect("");

    /* TFT ILI9341

    let sck = pins.gpio10.into_function::<FunctionSpi>();
    let mosi = pins.gpio11.into_function::<FunctionSpi>();
    let miso = pins.gpio12.into_function::<FunctionSpi>();

    let disp_reset = pins.gpio14.into_push_pull_output();
    let disp_dc = pins.gpio15.into_push_pull_output();
    let disp_cs = pins.gpio13.into_push_pull_output();

    let spi = Spi::<_,_,_,8>::new(pac.SPI1, (mosi, miso, sck))
    .init(
        &mut pac.RESETS,
        clocks.peripheral_clock.freq(),
        16_u32.MHz(),
        &embedded_hal::spi::MODE_0,
    );

    use embedded_hal_bus::spi::ExclusiveDevice;
    let spi_dev = ExclusiveDevice::new_no_delay(spi, NoCs).unwrap();

    let mut buffer = [0_u8; 1024];
    //use display_interface_spi::SPIInterface;

    let iface = SpiInterface::new(spi_dev, disp_dc, &mut buffer);
    let mut display = Builder::new(ILI9341Rgb565, iface)
        .reset_pin(disp_reset)
        .color_order(ColorOrder::Bgr)
        .init(&mut delay)
        .unwrap();

    // Set the display all red
    display.set_pixels(1, 1, 100, 100, [Rgb565::new(0xFF, 0, 0); 16384]);*/
    loop {
        if !device.poll(&mut [&mut midi]) {
            continue;
        }

        let mut buffer = [0_u8; 64];

        if let Ok(size) = midi.read(&mut buffer) {
            let packet_reader = UsbMidiPacketReader::new(&buffer, size);
            for packet in packet_reader.into_iter() {
                if let Ok(packet) = packet {
                    let _ = interpreter::process(packet, &mut dual_ym);
                }
            }
        }
    }
}

/// Noop `OutputPin` implementation.
///
/// This is passed to `ExclusiveDevice`, because the CS pin is handle in
/// hardware.
struct NoCs;

impl embedded_hal::digital::OutputPin for NoCs {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl embedded_hal::digital::ErrorType for NoCs {
    type Error = core::convert::Infallible;
}
/// Wrapper around `Delay` to implement the embedded-hal 1.0 delay.
///
/// This can be removed when a new version of the `cortex_m` crate is released.
struct DelayCompat(cortex_m::delay::Delay);

impl embedded_hal::delay::DelayNs for DelayCompat {
    fn delay_ns(&mut self, mut ns: u32) {
        while ns > 1000 {
            self.0.delay_us(1);
            ns = ns.saturating_sub(1000);
        }
    }

    fn delay_us(&mut self, us: u32) {
        self.0.delay_us(us);
    }
}
