use usbd_midi::{Message, message::{Channel}, packet::UsbMidiEventPacket};
use ym2149_core::{audio::AudioChannel, chip::YM2149, command::CommandOutput, errors::Error};

const CHANNELS: [AudioChannel; 3] = [AudioChannel::A, AudioChannel::B, AudioChannel::C];

fn parse_channel(c: Channel) -> Result<(AudioChannel, u8), Error> {
    let index = c.into();
    match index {
        0..5 => Ok((CHANNELS[index as usize], (index > 2) as u8 * 0xF)),
        _ => Err(Error::RegisterOutOfRange(index))
    }
}

pub fn process<C: CommandOutput>(packet: UsbMidiEventPacket, chip: &mut YM2149<C>) -> Result<(), Error> {
    match Message::try_from(&packet).unwrap() {
        Message::NoteOn(c, n, v) => {
            let (channel, register_offset) = parse_channel(c)?;
            chip.command_output.r

            let note_offset: Result<u8, _> = n.try_into();

            let _ = chip.tone_hz(channel, 440);
        }
        _ => {}
    }

    Ok(())
}
