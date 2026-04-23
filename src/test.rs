#![no_std]
#![no_main]
#![feature(lazy_type_alias)]

use cortex_m::asm::delay;
// Bootloader
use rp2040_boot2;
#[link_section = ".boot2"]
#[used]
pub static BOOT_LOADER: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;

const ADDRESS_MODE: u32 = 0xC000; // 15 (BDIR) & 14 (BC1) high
const WRITE_MODE: u32 = 0x8000; // 15 (BDIR) high, 14 (BC1) low

// Deps
use defmt_rtt as _;
use panic_halt as _;

use rp2040_hal::{self as hal, Spi, gpio::{AnyPin, FunctionSpi, PinState, PinGroup, ReadPinHList, WritePinHList}};
use rp2040_hal::fugit::RateExtU32;
use embedded_hal::digital::OutputPin;
use embedded_hal_bus::spi::ExclusiveDevice;

use mipidsi::{Builder, interface::SpiInterface};
use mipidsi::{models::ILI9341Rgb565, options::ColorOrder};           // Provides the builder for Display
use embedded_graphics::{pixelcolor::Rgb565, prelude::{OriginDimensions, WebColors}};

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
use ym2149_core::{chip::YM2149, command::{Command, CommandOutput}, io::{IoPortMode, IoPortMixerSettings}};

// MIDI command interpreter
mod interpreter;

pub struct DataBusController<H, T>
where
    HCons<H, T>: ReadPinHList + WritePinHList,
    H: AnyPin,
{
    pin_group: PinGroup<HCons<H, T>>,
    b_active: bool,
}



impl<H, T> DataBusController<H, T>
where
    HCons<H, T>: ReadPinHList + WritePinHList,
    H: AnyPin,
{
    pub fn new(pin_group: PinGroup<HCons<H, T>>) -> Self {
        Self {
            pin_group,
            b_active: false
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
        // write address & set inactive
        self.pin_group.set_u32(
            (ADDRESS_MODE << mode_shift) + ((command.register as u32) << 2) // Address mode on correct chip & write register on pins 2-9.
        );

        delay(1_000_000);

        // write value & set inactive
        self.pin_group.set_u32(
            (WRITE_MODE << mode_shift) + ((command.value as u32) << 2) // Write mode on correct chip & write value on pins 2-9.
        );

        delay(1_000_000);

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
    let mut delay = cortex_m::delay::Delay::new(
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

    // Frequency (in Hz, u32) of the clock the chip is connected to (Pin 22 on the YM2149)
    let master_clock_freq: u32 = 3_579_545;

    //let mut status_led = pins.gpio25.into_push_pull_output_in_state(PinState::High);


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
        .add_pin(pins.gpio17.into_push_pull_output()) // BC1 B

        .add_pin(pins.gpio25.into_push_pull_output());

    //let mut data_bus = DataBusController::new(
    //    ym_pins,
    //);

    //data_bus.b_active = true;

    // Build the chip by passing:
    //let mut dual_ym = YM2149::new(
    //    data_bus,
    //    master_clock_freq
    //).expect("");


    let mut reset = pins.gpio21.into_push_pull_output();

    reset.set_high();

    loop {
        /*dual_ym.setup_io_and_mixer(IoPortMixerSettings{
            noise_ch_a: true,
            noise_ch_b: true,
            noise_ch_c: true,
            tone_ch_a: true,
            tone_ch_b: true,
            tone_ch_c: true,
            ..Default::default()
        });

        dual_ym.level(ym2149_core::audio::AudioChannel::A, 0x0F);
        dual_ym.level(ym2149_core::audio::AudioChannel::B, 0x0F);
        dual_ym.level(ym2149_core::audio::AudioChannel::C, 0x0F);

        dual_ym.tone_hz(ym2149_core::audio::AudioChannel::A, 220);
        dual_ym.tone_hz(ym2149_core::audio::AudioChannel::B, 440);
        dual_ym.tone_hz(ym2149_core::audio::AudioChannel::C, 880);*/

        ym_pins.toggle();

        delay.delay_ms(1_000);
        /*if !device.poll(&mut [&mut midi]) {
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
        }*/
    }
}
