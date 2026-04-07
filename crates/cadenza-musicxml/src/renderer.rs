use cadenza_theory::{phrase::Phrase, rhythm::NoteEvent};
use crate::score::{Score, Part};

pub struct MusicXmlRenderer;

impl MusicXmlRenderer {
    pub fn from_phrase(phrase: &Phrase, title: &str, composer: &str) -> String {
        let mut xml = String::with_capacity(4096);
        xml.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
        xml.push('\n');
        xml.push_str(r#"<!DOCTYPE score-partwise PUBLIC "-//Recordare//DTD MusicXML 4.0 Partwise//EN" "http://www.musicxml.org/dtds/partwise.dtd">"#);
        xml.push('\n');
        xml.push_str(r#"<score-partwise version="4.0">"#);
        xml.push('\n');

        // Work / movement metadata
        xml.push_str(&format!("  <work><work-title>{}</work-title></work>\n", escape_xml(title)));
        xml.push_str(&format!(
            "  <identification><creator type=\"composer\">{}</creator></identification>\n",
            escape_xml(composer)
        ));

        // Part list
        xml.push_str("  <part-list>\n");
        xml.push_str("    <score-part id=\"P1\">\n");
        xml.push_str(&format!("      <part-name>{}</part-name>\n", escape_xml(&phrase.label)));
        xml.push_str("    </score-part>\n");
        xml.push_str("  </part-list>\n");

        // Single part
        xml.push_str("  <part id=\"P1\">\n");
        Self::write_measures(&mut xml, phrase);
        xml.push_str("  </part>\n");

        xml.push_str("</score-partwise>\n");
        xml
    }

    fn write_measures(xml: &mut String, phrase: &Phrase) {
        let tpb = phrase.time_sig.ticks_per_bar();
        let total_bars = phrase.bars as u32;
        let divisions = 480u32;   // MusicXML divisions = ticks per quarter

        for bar in 0..total_bars {
            let bar_start = bar * tpb;
            let bar_end = bar_start + tpb;

            xml.push_str(&format!("    <measure number=\"{}\">\n", bar + 1));

            // Attributes on first bar
            if bar == 0 {
                xml.push_str("      <attributes>\n");
                xml.push_str(&format!("        <divisions>{}</divisions>\n", divisions));
                if let Some(ref scale) = phrase.key {
                    xml.push_str(&Self::key_signature_xml(scale));
                }
                xml.push_str(&Self::time_sig_xml(&phrase.time_sig));
                xml.push_str("        <clef><sign>G</sign><line>2</line></clef>\n");
                xml.push_str("      </attributes>\n");

                // Tempo direction
                xml.push_str(&format!(
                    "      <direction placement=\"above\"><direction-type><metronome><beat-unit>quarter</beat-unit><per-minute>{}</per-minute></metronome></direction-type><sound tempo=\"{}\"/></direction>\n",
                    phrase.tempo, phrase.tempo
                ));
            }

            // Collect notes in this bar
            let bar_events: Vec<&NoteEvent> = phrase.events.iter()
                .filter(|e| e.start >= bar_start && e.start < bar_end)
                .collect();

            if bar_events.is_empty() {
                // Whole rest
                xml.push_str("      <note><rest measure=\"yes\"/><duration>1920</duration><type>whole</type></note>\n");
            } else {
                let mut cursor = bar_start;
                for ev in &bar_events {
                    // Fill gap with rest if needed
                    if ev.start > cursor {
                        Self::write_rest(xml, ev.start - cursor, divisions);
                    }
                    Self::write_note(xml, ev, divisions);
                    cursor = ev.start + ev.duration;
                }
            }

            xml.push_str("    </measure>\n");
        }
    }

    fn write_note(xml: &mut String, ev: &NoteEvent, _divisions: u32) {
        let (step, octave, alter) = midi_to_step(ev.pitch);
        let dur_type = ticks_to_type(ev.duration);

        xml.push_str("      <note>\n");
        xml.push_str("        <pitch>\n");
        xml.push_str(&format!("          <step>{}</step>\n", step));
        if alter != 0 {
            xml.push_str(&format!("          <alter>{}</alter>\n", alter));
        }
        xml.push_str(&format!("          <octave>{}</octave>\n", octave));
        xml.push_str("        </pitch>\n");
        xml.push_str(&format!("        <duration>{}</duration>\n", ev.duration));
        xml.push_str(&format!("        <type>{}</type>\n", dur_type));
        xml.push_str(&format!("        <dynamics>{}</dynamics>\n", vel_to_dynamic(ev.velocity)));
        xml.push_str("      </note>\n");
    }

    fn write_rest(xml: &mut String, ticks: u32, _divisions: u32) {
        let dur_type = ticks_to_type(ticks);
        xml.push_str(&format!(
            "      <note><rest/><duration>{}</duration><type>{}</type></note>\n",
            ticks, dur_type
        ));
    }

    fn key_signature_xml(scale: &cadenza_theory::scale::Scale) -> String {
        // Approximate sharps/flats from root + mode
        let fifths: i8 = match scale.root.0 {
            0  => 0,  // C
            2  => 2,  // D
            4  => 4,  // E
            5  => -1, // F
            7  => 1,  // G
            9  => 3,  // A
            11 => 5,  // B
            1  => -5, // Db
            3  => -3, // Eb
            6  => -6, // Gb
            8  => -4, // Ab
            10 => -2, // Bb
            _  => 0,
        };
        let mode = match scale.mode {
            cadenza_theory::scale::Mode::Major | cadenza_theory::scale::Mode::Lydian |
            cadenza_theory::scale::Mode::Mixolydian => "major",
            _ => "minor",
        };
        format!("        <key><fifths>{}</fifths><mode>{}</mode></key>\n", fifths, mode)
    }

    fn time_sig_xml(ts: &cadenza_theory::rhythm::TimeSignature) -> String {
        format!(
            "        <time><beats>{}</beats><beat-type>{}</beat-type></time>\n",
            ts.numerator, ts.denominator
        )
    }
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
     .replace('"', "&quot;").replace('\'', "&apos;")
}

fn midi_to_step(midi: u8) -> (&'static str, i8, i8) {
    let pc = midi % 12;
    let octave = (midi / 12) as i8 - 1;
    match pc {
        0  => ("C", octave, 0),
        1  => ("C", octave, 1),
        2  => ("D", octave, 0),
        3  => ("D", octave, 1),
        4  => ("E", octave, 0),
        5  => ("F", octave, 0),
        6  => ("F", octave, 1),
        7  => ("G", octave, 0),
        8  => ("G", octave, 1),
        9  => ("A", octave, 0),
        10 => ("A", octave, 1),
        11 => ("B", octave, 0),
        _  => ("C", octave, 0),
    }
}

fn ticks_to_type(ticks: u32) -> &'static str {
    match ticks {
        1920 => "whole", 960 => "half", 480 => "quarter",
        240  => "eighth", 120 => "16th", 60 => "32nd",
        320  => "quarter",  // triplet approximation
        160  => "eighth",
        _    => "quarter",
    }
}

fn vel_to_dynamic(vel: u8) -> &'static str {
    match vel {
        0..=31   => "pp",
        32..=63  => "p",
        64..=79  => "mp",
        80..=95  => "mf",
        96..=111 => "f",
        _        => "ff",
    }
}
