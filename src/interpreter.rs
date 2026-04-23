use crate::DataBusController;
use frunk::HCons;
use rp2040_hal::gpio::{AnyPin, ReadPinHList, WritePinHList};
use micromath::F32Ext;
use usbd_midi::{
    message::{Channel, ControlFunction},
    packet::UsbMidiEventPacket,
    Message,
};
use ym2149_core::{
    audio::AudioChannel,
    chip::YM2149,
    envelopes::{BuiltinEnvelopeShape, Envelope, EnvelopeFrequency},
    errors::Error,
};


const CHANNELS: [AudioChannel; 3] = [AudioChannel::A, AudioChannel::B, AudioChannel::C];

const ENVELOPE_SHAPES: [BuiltinEnvelopeShape; 5] = [
    BuiltinEnvelopeShape::FadeIn,
    BuiltinEnvelopeShape::FadeOut,
    BuiltinEnvelopeShape::Saw,
    BuiltinEnvelopeShape::Tooth,
    BuiltinEnvelopeShape::Triangle,
];

// So MIDI can send only 7 bits at a time. To control the envelope generator, we need 20 bits total.
// Hence, you need a very weird way of gluing them together from 2 separate messages.
// Thanks, 1983 MIDI protocol!
//
// xxCTLHR RRRR   RRRFFFF FFFF
// ||||||| ||||   ||||||| ||||
//   MSB    ch      LSB    ch
//
// x - unused
// C, T, L, H - CONT, ATT, ALT, HOLD
// R - rough adjustment
// F - fine adjustment
pub struct U20{
    pub value: u32
}

impl U20{
    fn get_envelope_shape(&mut self) -> u8 {
        (self.value >> 16 & 0x0F) as u8
    }

    fn get_rough_adj(&mut self) -> u8 {
        (self.value >> 8 & 0xFF) as u8
    }

    fn get_fine_adj(&mut self) -> u8 {
        (self.value & 0xFF) as u8
    }

    fn read(&mut self, ch: u8, v: u32, higher: bool) {
        let mask = 0xFF800 - (0xFF001 * (higher as u32));
        self.value = (self.value & mask) + (v << 4) + ch as u32
    }
}

fn parse_channel(c: Channel) -> Result<(AudioChannel, bool), Error> {
    let index = c.into();

    match index {
        0..5 => Ok((CHANNELS[(index % 3) as usize], index > 2)),
        15 => todo!("AUTOASSIGN"),
        _ => Err(Error::RegisterOutOfRange(index)),
    }
}

pub fn process<H, T>(
    packet: UsbMidiEventPacket,
    chip: &mut YM2149<DataBusController<H, T>>,
    buffer: &mut U20,
) -> Result<(), Error>
where
    HCons<H, T>: ReadPinHList + WritePinHList,
    H: AnyPin,
{
    match Message::try_from(&packet).unwrap() {
        Message::NoteOn(c, n, v) => {
            let (channel, b) = parse_channel(c)?;
            chip.command_output.b_active = b;
            let vel: u8 = v.into();

            let note_offset: u8 = n.try_into().unwrap(); // offset from A0
            let f: f32 = 27.5 * 2f32.powf(note_offset as f32 / 12.0);

            chip.tone_hz(channel, f as u32)?;
            chip.level(channel, vel / 8);
        }
        Message::NoteOff(c, _, _) => {
            let (channel, b) = parse_channel(c)?;
            chip.command_output.b_active = b;

            chip.level(channel, 0);
        }
        Message::ControlChange(c, f, v) => {
            let data: u8 = v.into();

            match f {
                ControlFunction::CHANNEL_VOLUME_7 => {
                    let (channel, b) = parse_channel(c)?;
                    chip.command_output.b_active = b;
                    chip.level(channel, data / 8);
                }
                ControlFunction::GENERAL_PURPOSE_CONTROLLER_1_16 => {
                    buffer.read(c.into(), data.into(), true);
                    chip.set_envelope_frequency(EnvelopeFrequency::Integer((buffer.value & 0xFFFF) as u16))?;
                }
                ControlFunction::LSB_FOR_GENERAL_PURPOSE_CONTROLLER_1_48 => {
                    buffer.read(c.into(), data.into(), false);
                }
                _ => {}
            }
        }

        Message::ProgramChange(c, v) => {
            let byte: u8 = v.into();
            match byte {
                1..=5 => {
                    let shape = ENVELOPE_SHAPES[(byte - 1) as usize];
                    chip.set_envelope_shape(&Envelope::Builtin(shape));
                }
                6..=10 => {
                    let shape = ENVELOPE_SHAPES[(byte - 6) as usize];
                    chip.set_envelope_shape(&Envelope::InvertedBuiltin(shape));
                }
                _ => {}
            }
        }
        _ => {}
    }

    Ok(())
}
