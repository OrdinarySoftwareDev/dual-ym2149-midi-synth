#![no_std]
#![no_main]

use cortex_m::{delay::Delay};

// Bootloader
use rp2040_boot2;
#[link_section = ".boot2"]
#[used]
pub static BOOT_LOADER: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;

const ADDRESS_MODE: u32 = 0xC000; // 15 (BDIR) & 14 (BC1) high
const WRITE_MODE: u32 = 0x8000; // 15 (BDIR) high, 14 (BC1) low

const T_AH: u32 = 80; // Address hold (> 80ns)
const T_AS: u32 = 300; // Address setup (> 300ns)

const T_RW: u32 = 1_000; // Reset pulse width (> 500ns)
const T_RB: u32 = 1_000; // Reset bus control delay time (> 100ns)

const T_DS: u32 = 2; // Data setup (> 0ns)
const T_DW: u32 = 300; // Write signal (valid range 300ns - 10us)
const T_DH: u32 = 80; // Data hold (> 80ns)

// Deps
use defmt_rtt as _;
use panic_halt as _;

use rp2040_hal::{self as hal, Timer, gpio::{AnyPin, PinGroup, PinState, ReadPinHList, WritePinHList}};
use rp2040_hal::fugit::RateExtU32;
use embedded_hal::{delay::DelayNs, digital::OutputPin};
use embedded_hal_bus::spi::ExclusiveDevice;

use mipidsi::{Builder, interface::SpiInterface};
use embedded_graphics::{pixelcolor::Rgb565};

use hal::{
    clocks::{init_clocks_and_plls},
    Clock,
    pac,
    sio::Sio,
    watchdog::Watchdog,
};

use usb_device::{class_prelude::*, prelude::*};

use usbd_midi::{
    UsbMidiClass,
    UsbMidiPacketReader,
};

use frunk::{HCons};

// YM2149 driver
use ym2149_core::{chip::YM2149, command::{Command, CommandOutput}};

// MIDI command interpreter
mod interpreter;

pub struct DataBusController<H, T>
where
    HCons<H, T>: ReadPinHList + WritePinHList,
    H: AnyPin,
{
    pin_group: PinGroup<HCons<H, T>>,
    b_active: bool,
    timer: Timer,
}



impl<H, T> DataBusController<H, T>
where
    HCons<H, T>: ReadPinHList + WritePinHList,
    H: AnyPin,
{
    pub fn new(pin_group: PinGroup<HCons<H, T>>, timer: Timer) -> Self {
        Self {
            pin_group,
            b_active: false,
            timer
        }
    }
}

impl<H, T> CommandOutput for DataBusController<H, T>
where
    HCons<H, T>: ReadPinHList + WritePinHList,
    H: AnyPin,
{
    fn execute(&mut self, command: Command) {
        let mode_shift = (self.b_active as u8) * 2;

        /*
         *  ADDRESS
         */
        self.pin_group.set_u32(
            ADDRESS_MODE << mode_shift  // address mode
        );

        self.timer.delay_ns(T_AH);

        self.pin_group.set_u32(
            (ADDRESS_MODE << mode_shift) + ((command.register as u32) << 2)  // address mode & address data
        );

        self.timer.delay_ns(T_AS);

        self.pin_group.set_u32(
            (command.register as u32) << 2  // inactive mode & address data
        );

        self.timer.delay_ns(T_AH);

        self.pin_group.set_u32(0); // inactive

        self.timer.delay_ns(500);

        /*
         *  DATA
         */
        self.pin_group.set_u32(
            (command.value as u32) << 2 // data
        );

        self.timer.delay_ns(T_DS);

        self.pin_group.set_u32(
            (WRITE_MODE << mode_shift) + ((command.value as u32) << 2) // write mode & data
        );

        self.timer.delay_ns(T_DW);

        self.pin_group.set_u32(
            (command.value as u32) << 2 // data
        );

        self.timer.delay_ns(T_DH);

        self.pin_group.set_u32(0); // inactive

        self.timer.delay_ns(500);
    }
}

use embedded_graphics::{
    image::{Image},
    mono_font::{
        MonoTextStyle, MonoTextStyleBuilder,
        ascii::{FONT_5X7, FONT_8X13_BOLD},
    },
    prelude::*,
    text::{Alignment, Text},
};
use tinybmp::Bmp;

use mipidsi::{models::ST7789};

// Preset dimensions

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;

const H_CENTER: i32 = WIDTH as i32 / 2;
const V_CENTER: i32 = HEIGHT as i32 / 2;

// Preset points
const NOISE_REG: Point = Point::new(51, 169);
const ENV_REG: Point = Point::new(51, 182);

const CONSOLE: Point = Point::new(125, 43);

use mipidsi::options::ColorInversion;

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
    let mut delay = Delay::new(
        core.SYST,
        clocks.system_clock.freq().to_Hz(),
    );

    let pins = hal::gpio::Pins::new(
        pac.IO_BANK0,
        pac.PADS_BANK0,
        sio.gpio_bank0,
        &mut pac.RESETS,
    );

    // actual code
    let usb_bus = UsbBusAllocator::new(hal::usb::UsbBus::new(
        pac.USBCTRL_REGS,
        pac.USBCTRL_DPRAM,
        clocks.usb_clock,
        true,
        &mut pac.RESETS
    ));

    let mut midi = UsbMidiClass::new(&usb_bus, 0, 1).unwrap();

    let mut device = UsbDeviceBuilder::new(&usb_bus, UsbVidPid(0x2E8A, 0x000A))
        .device_class(0)
        .device_sub_class(0)
        .strings(&[StringDescriptors::default()
            .manufacturer("vw.dvw")
            .product("Dual YM2149 USB-MIDI Synthesizer")
            .serial_number("DZ-0001")])
        .unwrap()
        .build();

    /*/ Set up our SPI pins so they can be used by the SPI driver
    let spi_mosi = pins.gpio11.into_function::<hal::gpio::FunctionSpi>();
    let spi_miso = pins.gpio12.into_function::<hal::gpio::FunctionSpi>();
    let spi_sclk = pins.gpio10.into_function::<hal::gpio::FunctionSpi>();
    let spi_bus = hal::spi::Spi::<_, _, _, 8>::new(pac.SPI1, (spi_mosi, spi_miso, spi_sclk));

    // Exchange the uninitialised SPI driver for an initialised one
    let spi_bus = spi_bus.init(
        &mut pac.RESETS,
        clocks.peripheral_clock.freq(),
        16.MHz(),
        embedded_hal::spi::MODE_0,
    );

    let cs = pins.gpio13.into_push_pull_output();

    let spi_device = ExclusiveDevice::new_no_delay(spi_bus, cs).unwrap();
    let mut buffer = [0_u8; 512];
    let dc = pins.gpio18.into_push_pull_output();
    let di = SpiInterface::new(spi_device, dc, &mut buffer);


    let mut display = Builder::new(ST7789, di)
        .display_size(WIDTH as u16, HEIGHT as u16)
        .invert_colors(ColorInversion::Inverted)
        .init(&mut DelayCompat(delay))
        .unwrap();

    let bmp: Bmp<Rgb565> = Bmp::from_slice(include_bytes!("../artboard.bmp")).unwrap();

    let image = Image::new(&bmp, Point::new(0, -1));
    image.draw(&mut display);

    let status = Text::with_alignment(
        "USB CONNECTED",
        Point::new(H_CENTER, 13),
        MonoTextStyle::new(&FONT_8X13_BOLD, Rgb565::BLACK),
        Alignment::Center,
    );

    status.draw(&mut display);

    let firmware_version = Text::with_alignment(
        "FIRMWARE VERSION 0.1a",
        Point::new(H_CENTER, 240 - 10),
        MonoTextStyle::new(&FONT_5X7, Rgb565::WHITE),
        Alignment::Center,
    );

    firmware_version.draw(&mut display);

    let console_text_style = MonoTextStyleBuilder::<Rgb565>::new()
        .font(&FONT_5X7)
        .text_color(Rgb565::WHITE)
        .build();

    let console_text = Text::with_baseline(
        "$ ON(A, A4, 127) => [\n. 0x0 0x0;\n. 0x1 0xC1;\n. 0x8 0xF\n. ]\n$ OFF(B) => 0x9 0x0;\n$ CC(A, 7, 63) => 0x8 0x7;\n$",
        CONSOLE,
        console_text_style,
        embedded_graphics::text::Baseline::Top,
    );

    console_text.draw(&mut display);*/


    // Frequency (in Hz, u32) of the clock the chip is connected to (Pin 22 on the YM2149)
    let master_clock_freq: u32 = 3_579_545 / 2;

    let mut status_led = pins.gpio25.into_push_pull_output_in_state(PinState::High);


    // Initialize a PinGroup
    let mut ym_pins = PinGroup::new()
        .add_pin(pins.gpio2.into_push_pull_output()) // DATA BUS
        .add_pin(pins.gpio3.into_push_pull_output())
        .add_pin(pins.gpio4.into_push_pull_output())
        .add_pin(pins.gpio5.into_push_pull_output())
        .add_pin(pins.gpio6.into_push_pull_output())
        .add_pin(pins.gpio7.into_push_pull_output())
        .add_pin(pins.gpio8.into_push_pull_output())
        .add_pin(pins.gpio9.into_push_pull_output())

        .add_pin(pins.gpio14.into_push_pull_output()) // BDIR A
        .add_pin(pins.gpio15.into_push_pull_output()) // BC1 A
        .add_pin(pins.gpio16.into_push_pull_output()) // BDIR B
        .add_pin(pins.gpio17.into_push_pull_output()); // BC1 B

    ym_pins.set_u32(0);

    let mut reset = pins.gpio21.into_push_pull_output();
    reset.set_high();
    timer.delay_ns(T_RW);

    reset.set_low();
    timer.delay_ns(T_RW);

    reset.set_high();
    timer.delay_ns(T_RB);


    let data_bus = DataBusController::new(
        ym_pins,
        timer
    );

    // Build the chip by passing:
    let mut dual_ym = YM2149::new(
        data_bus,
        master_clock_freq
    ).expect("");

    loop {
        if !device.poll(&mut [&mut midi]) {
            continue;
        }

        let mut buffer = [0_u8; 64];
        let mut ymb = interpreter::U20 {
            value: 0
        };

        if let Ok(size) = midi.read(&mut buffer) {
            let packet_reader = UsbMidiPacketReader::new(&buffer, size);
            for packet in packet_reader.into_iter() {
                if let Ok(packet) = packet {
                    let _ = interpreter::process(packet, &mut dual_ym, &mut ymb);
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
