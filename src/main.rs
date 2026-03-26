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

use embedded_hal::digital::{
    OutputPin,
    PinState::{self, *},
};
use rp2040_hal::fugit::RateExtU32;
use rp2040_hal::{
    self as hal,
    gpio::{AnyPin, FunctionSpi, PinGroup},
    Spi,
};

use embedded_graphics::{pixelcolor::Rgb666, prelude::*};
use mipidsi::models::ILI9341Rgb565; // Provides the builder for Display
use mipidsi::{interface::SpiInterface, Builder};

use hal::{clocks::init_clocks_and_plls, pac, sio::Sio, watchdog::Watchdog, Clock};

use usb_device::{class_prelude::*, prelude::*};
use usbd_serial::SerialPort;

// YM2149 driver
use ym2149_core::{
    chip::YM2149,
    command::{Command, CommandOutput},
};

use frunk::HCons;

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

#[hal::entry]
fn main() -> ! {
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
    let mut delay = cortex_m::delay::Delay::new(core.SYST, clocks.system_clock.freq().to_Hz());

    let pins = hal::gpio::Pins::new(
        pac.IO_BANK0,
        pac.PADS_BANK0,
        sio.gpio_bank0,
        &mut pac.RESETS,
    );

    let mut status_led = pins.gpio25.into_push_pull_output();

    let usb_bus = UsbBusAllocator::new(hal::usb::UsbBus::new(
        pac.USBCTRL_REGS,
        pac.USBCTRL_DPRAM,
        clocks.usb_clock,
        true,
        &mut pac.RESETS,
    ));

    let mut serial = SerialPort::new(&usb_bus);

    let mut device = UsbDeviceBuilder::new(&usb_bus, UsbVidPid(0x2E8A, 0x000A))
        .strings(&[StringDescriptors::default()
            .manufacturer("vw.dvw")
            .product("Dual YM2149 USB-MIDI Synthesizer")])
        .unwrap()
        .device_class(2)
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

    // TFT ILI9341

    let sck = pins.gpio10.into_function::<FunctionSpi>();
    let mosi = pins.gpio11.into_function::<FunctionSpi>();
    let miso = pins.gpio12.into_function::<FunctionSpi>();

    //let disp_reset = pins.gpio14.into_push_pull_output();
    let disp_dc = pins.gpio15.into_push_pull_output();
    let disp_cs = pins.gpio13.into_push_pull_output();

    let spi = Spi::<_, _, _, 8>::new(pac.SPI1, (mosi, miso, sck)).init(
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
    let mut display = Builder::new(ILI9341Rgb565, iface);

    loop {
        if device.poll(&mut [&mut serial]) {
            let mut buf = [0u8; 2];
            if let Ok(_) = serial.read(&mut buf) {
                let (register, value) = (buf[0], buf[1]);
                // dual_ym.command(register, value);
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
