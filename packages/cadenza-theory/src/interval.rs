use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interval(pub i8);

impl Interval {
    pub const UNISON:          Self = Self(0);
    pub const MINOR_SECOND:    Self = Self(1);
    pub const MAJOR_SECOND:    Self = Self(2);
    pub const MINOR_THIRD:     Self = Self(3);
    pub const MAJOR_THIRD:     Self = Self(4);
    pub const PERFECT_FOURTH:  Self = Self(5);
    pub const TRITONE:         Self = Self(6);
    pub const PERFECT_FIFTH:   Self = Self(7);
    pub const MINOR_SIXTH:     Self = Self(8);
    pub const MAJOR_SIXTH:     Self = Self(9);
    pub const MINOR_SEVENTH:   Self = Self(10);
    pub const MAJOR_SEVENTH:   Self = Self(11);
    pub const OCTAVE:          Self = Self(12);

    pub fn semitones(&self) -> i8 { self.0 }
    pub fn abs(&self) -> u8 { self.0.unsigned_abs() }
    pub fn is_tritone(&self) -> bool { self.0.abs() == 6 }
    pub fn negate(&self) -> Self { Self(-self.0) }
}
