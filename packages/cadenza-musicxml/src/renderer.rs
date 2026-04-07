use cadenza_theory::{phrase::Phrase, rhythm::NoteEvent, scale::Scale};
use std::collections::{BTreeMap, BTreeSet};

pub struct MusicXmlRenderer;

/// A note segment after barline-splitting. Slur and tie state is resolved
/// against the original (unsplit) parent events.
#[derive(Debug, Clone)]
struct Segment {
    pitch: u8,
    start: u32,
    duration: u32,
    voice: u8,
    tie_start: bool,
    tie_stop: bool,
    slur_start: bool,
    slur_stop: bool,
    parent_idx: usize,
}

impl MusicXmlRenderer {
    pub fn from_phrase(phrase: &Phrase, title: &str, composer: &str) -> String {
        let mut x = String::with_capacity(4096);
        x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        x.push_str("<!DOCTYPE score-partwise PUBLIC \"-//Recordare//DTD MusicXML 4.0 Partwise//EN\" \"http://www.musicxml.org/dtds/partwise.dtd\">\n");
        x.push_str("<score-partwise version=\"4.0\">\n");
        x.push_str(&format!("  <work><work-title>{}</work-title></work>\n", esc(title)));
        x.push_str(&format!("  <identification><creator type=\"composer\">{}</creator></identification>\n", esc(composer)));
        x.push_str("  <part-list>\n    <score-part id=\"P1\">\n");
        x.push_str(&format!("      <part-name>{}</part-name>\n", esc(&phrase.label)));
        x.push_str("    </score-part>\n  </part-list>\n  <part id=\"P1\">\n");
        Self::write_measures(&mut x, phrase);
        x.push_str("  </part>\n</score-partwise>\n");
        x
    }

    fn write_measures(x: &mut String, phrase: &Phrase) {
        let tpb = phrase.time_sig.ticks_per_bar();
        let segments = build_segments(phrase);
        let lyric_lookup = build_lyric_lookup(phrase);

        for bar in 0..phrase.bars as u32 {
            let (bs, be) = (bar * tpb, (bar + 1) * tpb);
            x.push_str(&format!("    <measure number=\"{}\">\n", bar + 1));
            if bar == 0 {
                x.push_str("      <attributes>\n        <divisions>480</divisions>\n");
                if let Some(ref s) = phrase.key { x.push_str(&Self::key_xml(s)); }
                x.push_str(&format!("        <time><beats>{}</beats><beat-type>{}</beat-type></time>\n",
                    phrase.time_sig.numerator, phrase.time_sig.denominator));
                x.push_str("        <clef><sign>G</sign><line>2</line></clef>\n      </attributes>\n");
                x.push_str(&format!(
                    "      <direction placement=\"above\"><direction-type><metronome><beat-unit>quarter</beat-unit><per-minute>{}</per-minute></metronome></direction-type><sound tempo=\"{}\"/></direction>\n",
                    phrase.tempo, phrase.tempo));
            }

            // Group segments in this bar by voice (BTreeMap keeps voice order stable).
            let mut by_voice: BTreeMap<u8, Vec<&Segment>> = BTreeMap::new();
            for seg in &segments {
                if seg.start >= bs && seg.start < be {
                    by_voice.entry(seg.voice).or_default().push(seg);
                }
            }

            if by_voice.is_empty() {
                x.push_str("      <note><rest measure=\"yes\"/><duration>1920</duration><type>whole</type></note>\n");
            } else {
                let voice_count = by_voice.len();
                for (idx, (voice, segs)) in by_voice.iter().enumerate() {
                    let mut cur = bs;
                    for seg in segs {
                        if seg.start > cur { Self::write_rest(x, seg.start - cur, *voice); }
                        Self::write_note(x, seg, &lyric_lookup);
                        cur = seg.start + seg.duration;
                    }
                    // Pad voice to bar end with rests so backup math is exact.
                    if cur < be { Self::write_rest(x, be - cur, *voice); }
                    // Backup before next voice.
                    if idx + 1 < voice_count {
                        x.push_str(&format!(
                            "      <backup><duration>{}</duration></backup>\n",
                            be - bs
                        ));
                    }
                }
            }
            x.push_str("    </measure>\n");
        }
    }

    fn write_note(x: &mut String, seg: &Segment, lyrics: &[Option<(String, &'static str)>]) {
        let (step, oct, alter) = midi_to_step(seg.pitch);
        x.push_str("      <note>\n        <pitch>\n");
        x.push_str(&format!("          <step>{step}</step>\n"));
        if alter != 0 { x.push_str(&format!("          <alter>{alter}</alter>\n")); }
        x.push_str(&format!("          <octave>{oct}</octave>\n        </pitch>\n"));
        if seg.tie_stop  { x.push_str("        <tie type=\"stop\"/>\n"); }
        if seg.tie_start { x.push_str("        <tie type=\"start\"/>\n"); }
        x.push_str(&format!("        <duration>{}</duration>\n", seg.duration));
        x.push_str(&format!("        <voice>{}</voice>\n", seg.voice));
        x.push_str(&format!("        <type>{}</type>\n", ticks_to_type(seg.duration)));

        let has_notations = seg.tie_start || seg.tie_stop || seg.slur_start || seg.slur_stop;
        if has_notations {
            x.push_str("        <notations>\n");
            if seg.tie_stop  { x.push_str("          <tied type=\"stop\"/>\n"); }
            if seg.tie_start { x.push_str("          <tied type=\"start\"/>\n"); }
            if seg.slur_start { x.push_str("          <slur type=\"start\" number=\"1\"/>\n"); }
            if seg.slur_stop  { x.push_str("          <slur type=\"stop\" number=\"1\"/>\n"); }
            x.push_str("        </notations>\n");
        }
        if !seg.tie_stop {
            if let Some(Some((text, syllabic))) = lyrics.get(seg.parent_idx) {
                x.push_str("        <lyric number=\"1\">\n");
                x.push_str(&format!("          <syllabic>{syllabic}</syllabic>\n"));
                x.push_str(&format!("          <text>{}</text>\n", esc(text)));
                x.push_str("        </lyric>\n");
            }
        }
        x.push_str("      </note>\n");
    }

    fn write_rest(x: &mut String, ticks: u32, voice: u8) {
        x.push_str(&format!(
            "      <note><rest/><duration>{ticks}</duration><voice>{voice}</voice><type>{}</type></note>\n",
            ticks_to_type(ticks)
        ));
    }

    fn key_xml(scale: &Scale) -> String {
        let fifths: i8 = match scale.root.0 {
            0=>0, 2=>2, 4=>4, 5=>-1, 7=>1, 9=>3, 11=>5,
            1=>-5, 3=>-3, 6=>-6, 8=>-4, 10=>-2, _=>0,
        };
        let mode = match scale.mode {
            cadenza_theory::scale::Mode::Major|cadenza_theory::scale::Mode::Lydian|
            cadenza_theory::scale::Mode::Mixolydian => "major",
            _ => "minor",
        };
        format!("        <key><fifths>{fifths}</fifths><mode>{mode}</mode></key>\n")
    }
}

/// Split events at barlines and resolve slur start/stop markers per voice.
fn build_segments(phrase: &Phrase) -> Vec<Segment> {
    let tpb = phrase.time_sig.ticks_per_bar();

    // Resolve which parent events get slur_start / slur_stop. A "slur run" is
    // a maximal sequence of consecutive (by start time) notes within the same
    // voice that share the same Some(slur_group). Runs of length 1 don't emit
    // slur markers.
    let mut slur_starts: BTreeSet<usize> = BTreeSet::new();
    let mut slur_stops:  BTreeSet<usize> = BTreeSet::new();

    let mut by_voice: BTreeMap<u8, Vec<usize>> = BTreeMap::new();
    for (i, ev) in phrase.events.iter().enumerate() {
        by_voice.entry(ev.voice).or_default().push(i);
    }
    for idxs in by_voice.values_mut() {
        idxs.sort_by_key(|&i| phrase.events[i].start);
        let mut i = 0;
        while i < idxs.len() {
            let g = phrase.events[idxs[i]].slur_group;
            if g.is_some() {
                let run_start = i;
                while i + 1 < idxs.len() && phrase.events[idxs[i + 1]].slur_group == g {
                    i += 1;
                }
                if i > run_start {
                    slur_starts.insert(idxs[run_start]);
                    slur_stops.insert(idxs[i]);
                }
            }
            i += 1;
        }
    }

    let mut segments = Vec::with_capacity(phrase.events.len());
    for (i, ev) in phrase.events.iter().enumerate() {
        push_segments(&mut segments, i, ev, tpb, &slur_starts, &slur_stops);
    }
    segments.sort_by_key(|s| (s.start, s.voice));
    segments
}

fn push_segments(
    out: &mut Vec<Segment>,
    parent_idx: usize,
    ev: &NoteEvent,
    tpb: u32,
    slur_starts: &BTreeSet<usize>,
    slur_stops:  &BTreeSet<usize>,
) {
    let voice = if ev.voice == 0 { 1 } else { ev.voice };
    let starts_slur = slur_starts.contains(&parent_idx);
    let stops_slur  = slur_stops.contains(&parent_idx);

    let end = ev.start + ev.duration;
    let mut s = ev.start;
    let mut first = true;
    while s < end {
        let bar_end = ((s / tpb) + 1) * tpb;
        let seg_end = end.min(bar_end);
        let is_last = seg_end == end;
        out.push(Segment {
            pitch: ev.pitch,
            start: s,
            duration: seg_end - s,
            voice,
            tie_start: !is_last,
            tie_stop:  !first,
            slur_start: first  && starts_slur,
            slur_stop:  is_last && stops_slur,
            parent_idx,
        });
        s = seg_end;
        first = false;
    }
}

/// Build a per-event lookup of `(text, syllabic)` from the phrase's first
/// lyric line. Padding rests (`word_index < 0` or empty text) and out-of-range
/// `note_id`s are skipped. Syllabic kind is derived from `word_index` adjacency
/// in the line: same as previous → continuation, same as next → has more.
fn build_lyric_lookup(phrase: &Phrase) -> Vec<Option<(String, &'static str)>> {
    let mut out = vec![None; phrase.events.len()];
    let Some(line) = phrase.lyrics.first() else { return out };
    let syls = &line.syllables;
    for (i, syl) in syls.iter().enumerate() {
        if syl.word_index < 0 || syl.text.is_empty() { continue; }
        let prev_same = i > 0
            && syls[i - 1].word_index == syl.word_index
            && syls[i - 1].word_index >= 0;
        let next_same = i + 1 < syls.len()
            && syls[i + 1].word_index == syl.word_index
            && syls[i + 1].word_index >= 0;
        let syllabic = match (prev_same, next_same) {
            (false, false) => "single",
            (false, true)  => "begin",
            (true,  true)  => "middle",
            (true,  false) => "end",
        };
        let idx = syl.note_id as usize;
        if idx < out.len() {
            out[idx] = Some((syl.text.clone(), syllabic));
        }
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;")
     .replace('"',"&quot;").replace('\'',"&apos;")
}
fn midi_to_step(midi: u8) -> (&'static str, i8, i8) {
    let oct = (midi/12) as i8 - 1;
    match midi%12 {
        0=>("C",oct,0), 1=>("C",oct,1), 2=>("D",oct,0), 3=>("D",oct,1),
        4=>("E",oct,0), 5=>("F",oct,0), 6=>("F",oct,1), 7=>("G",oct,0),
        8=>("G",oct,1), 9=>("A",oct,0), 10=>("A",oct,1), _=>("B",oct,0),
    }
}
fn ticks_to_type(t: u32) -> &'static str {
    match t { 1920=>"whole", 960=>"half", 480=>"quarter", 240=>"eighth", 120=>"16th", _=>"quarter" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadenza_theory::rhythm::TimeSignature;

    fn ev(pitch: u8, start: u32, duration: u32, voice: u8, slur: Option<u8>) -> NoteEvent {
        NoteEvent { pitch, start, duration, velocity: 90, channel: 0, voice, slur_group: slur }
    }

    fn make_phrase(events: Vec<NoteEvent>, bars: u8) -> Phrase {
        let mut p = Phrase::new(0, "test", TimeSignature::four_four(), 120);
        p.bars = bars;
        p.events = events;
        p
    }

    #[test]
    fn note_crossing_barline_is_split_and_tied() {
        // Half note (960 ticks) starting at tick 1440 in a 4/4 bar (1920 ticks):
        // first segment 480 ticks in bar 1, second segment 480 ticks in bar 2.
        let p = make_phrase(vec![ev(60, 1440, 960, 1, None)], 2);
        let xml = MusicXmlRenderer::from_phrase(&p, "t", "c");

        // Two <note> elements with the same pitch (one per bar).
        assert_eq!(xml.matches("<step>C</step>").count(), 2);
        // Tie start in bar 1 segment, tie stop in bar 2 segment.
        assert!(xml.contains("<tie type=\"start\"/>"));
        assert!(xml.contains("<tie type=\"stop\"/>"));
        assert!(xml.contains("<tied type=\"start\"/>"));
        assert!(xml.contains("<tied type=\"stop\"/>"));

        // Both halves should be a quarter note in length now.
        let quarters = xml.matches("<duration>480</duration>").count();
        assert!(quarters >= 2, "expected at least two 480-tick segments, got xml:\n{xml}");

        // The tie-start segment must come before the tie-stop segment.
        let start_pos = xml.find("<tie type=\"start\"/>").unwrap();
        let stop_pos  = xml.find("<tie type=\"stop\"/>").unwrap();
        assert!(start_pos < stop_pos);
    }

    #[test]
    fn two_voices_in_one_measure_use_backup() {
        // Voice 1: whole-bar C. Voice 2: whole-bar E.
        let p = make_phrase(
            vec![
                ev(60, 0, 1920, 1, None),
                ev(64, 0, 1920, 2, None),
            ],
            1,
        );
        let xml = MusicXmlRenderer::from_phrase(&p, "t", "c");

        assert!(xml.contains("<voice>1</voice>"));
        assert!(xml.contains("<voice>2</voice>"));
        // Exactly one backup of a full bar between the two voices.
        assert_eq!(xml.matches("<backup><duration>1920</duration></backup>").count(), 1);
        // Voice 1 must appear before the backup, voice 2 after.
        let backup_pos = xml.find("<backup>").unwrap();
        let v1_pos = xml.find("<voice>1</voice>").unwrap();
        let v2_pos = xml.find("<voice>2</voice>").unwrap();
        assert!(v1_pos < backup_pos);
        assert!(v2_pos > backup_pos);
    }

    #[test]
    fn lyric_line_emits_syllabic_kinds_in_order() {
        use cadenza_theory::phrase::{LyricLine, LyricSyllable};
        // 4 quarter notes in a 4/4 bar.
        let mut p = make_phrase(
            vec![
                ev(60, 0,    480, 1, None),
                ev(62, 480,  480, 1, None),
                ev(64, 960,  480, 1, None),
                ev(65, 1440, 480, 1, None),
            ],
            1,
        );
        // "Hel-lo world !" → wordIndices [0,0,1,2]
        p.lyrics.push(LyricLine {
            phrase_id: 0,
            raw_text: "Hello world !".into(),
            syllables: vec![
                LyricSyllable { text: "Hel".into(), note_id: 0, word_index: 0, stress: "strong".into() },
                LyricSyllable { text: "lo".into(),  note_id: 1, word_index: 0, stress: "weak".into() },
                LyricSyllable { text: "world".into(), note_id: 2, word_index: 1, stress: "strong".into() },
                LyricSyllable { text: "!".into(),   note_id: 3, word_index: 2, stress: "unstressed".into() },
            ],
        });

        let xml = MusicXmlRenderer::from_phrase(&p, "t", "c");
        // Order matters: begin, end, single, single.
        let begin  = xml.find("<syllabic>begin</syllabic>").expect("begin");
        let end    = xml.find("<syllabic>end</syllabic>").expect("end");
        let single1 = xml.find("<syllabic>single</syllabic>").expect("single1");
        let single2 = xml.rfind("<syllabic>single</syllabic>").unwrap();
        assert!(begin < end);
        assert!(end < single1);
        assert!(single1 < single2);
        assert!(xml.contains("<text>Hel</text>"));
        assert!(xml.contains("<text>lo</text>"));
        assert!(xml.contains("<text>world</text>"));
    }

    #[test]
    fn lyric_skips_padding_rest_syllables() {
        use cadenza_theory::phrase::{LyricLine, LyricSyllable};
        let mut p = make_phrase(
            vec![
                ev(60, 0, 480, 1, None),
                ev(62, 480, 480, 1, None),
            ],
            1,
        );
        p.lyrics.push(LyricLine {
            phrase_id: 0,
            raw_text: "hi".into(),
            syllables: vec![
                LyricSyllable { text: "hi".into(), note_id: 0, word_index: 0, stress: "strong".into() },
                LyricSyllable { text: "".into(),   note_id: 1, word_index: -1, stress: "unstressed".into() },
            ],
        });
        let xml = MusicXmlRenderer::from_phrase(&p, "t", "c");
        assert_eq!(xml.matches("<lyric").count(), 1);
        assert!(xml.contains("<text>hi</text>"));
    }

    #[test]
    fn slur_run_emits_start_and_stop_only_at_endpoints() {
        let p = make_phrase(
            vec![
                ev(60, 0,    480, 1, Some(1)),
                ev(62, 480,  480, 1, Some(1)),
                ev(64, 960,  480, 1, Some(1)),
                ev(65, 1440, 480, 1, None),
            ],
            1,
        );
        let xml = MusicXmlRenderer::from_phrase(&p, "t", "c");
        assert_eq!(xml.matches("<slur type=\"start\"").count(), 1);
        assert_eq!(xml.matches("<slur type=\"stop\"").count(), 1);
    }
}
