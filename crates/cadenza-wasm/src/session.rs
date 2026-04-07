use std::collections::HashMap;
use cadenza_theory::phrase::Phrase;
use cadenza_theory::scale::Scale;
use cadenza_theory::rhythm::TimeSignature;

pub struct Session {
    pub phrases: HashMap<u32, Phrase>,
    pub next_id: u32,
    pub key: Option<Scale>,
    pub tempo: u16,
    pub time_sig: TimeSignature,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            phrases: HashMap::new(),
            next_id: 1,
            key: None,
            tempo: 120,
            time_sig: TimeSignature::four_four(),
        }
    }
}

impl Session {
    pub fn add_phrase(&mut self, mut phrase: Phrase) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        phrase.id = id;
        phrase.key = phrase.key.or_else(|| self.key.clone());
        phrase.tempo = if phrase.tempo == 0 { self.tempo } else { phrase.tempo };
        self.phrases.insert(id, phrase);
        id
    }

    pub fn get(&self, id: u32) -> Option<&Phrase> {
        self.phrases.get(&id)
    }
}
