/// This helper struct tracks the current state of the YM2149's channels.
#[derive(Debug, Clone, Copy)]
pub struct AudioChannelData {
    pub address: u8,
    pub enabled: bool,
    pub noise_enabled: bool,
    pub level: u8,
    pub pitch_bend: f32,
    pub last_note: Option<Note>,
}

impl AudioChannelData {
    /// Creates a new `AudioChannelData` for the channel at the given register address.
    pub fn new(address: u8) -> Self {
        Self {
            address: address,
            enabled: false,
            noise_enabled: false,
            level: 0,
            pitch_bend: 0.0,
            last_note: None,
        }
    }

    /// Set the channel's pitch bend.
    #[allow(unused)]
    pub fn set_pitch_bend(&mut self, byte1: u8, byte2: u8) {
        let new: f32 = (((byte2 as u16) << 7) + byte1 as u16).into();
        let as_semitones: f32 = (new - 8192.0) / 1024.0;
        self.pitch_bend = as_semitones;
    }
}
