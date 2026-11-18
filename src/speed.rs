use std::time::{Duration, Instant};

use crate::article::{BlockKind, WrappedLine};

pub const DEFAULT_WPM: u16 = 300;
pub const MIN_WPM: u16 = 100;
pub const MAX_WPM: u16 = 1000;
/// How much the `+` and `-` keys change the pace.
pub const WPM_STEP: i16 = 25;

/// Rapid serial visual presentation state for the reading view.
///
/// The article is tokenized into words and shown one at a time at a fixed
/// pace, starting at the line the reader is on.
#[derive(Debug, Clone)]
pub struct SpeedReader {
    words: Vec<String>,
    index: usize,
    wpm: u16,
    playing: bool,
    next_due: Instant,
}

impl SpeedReader {
    pub fn new(
        lines: &[WrappedLine],
        from_line: usize,
        wpm: u16,
    ) -> Option<Self> {
        Self::new_at(lines, from_line, wpm, Instant::now())
    }

    pub fn new_at(
        lines: &[WrappedLine],
        from_line: usize,
        wpm: u16,
        now: Instant,
    ) -> Option<Self> {
        let mut words = Vec::new();
        let mut start = None;
        for (line_index, line) in lines.iter().enumerate() {
            // Rendered images and their placeholders are not words.
            if line.kind == Some(BlockKind::Image) {
                continue;
            }
            let mut line_words = line.text.split_whitespace();
            if start.is_none() && line_index >= from_line {
                if let Some(first) = line_words.next() {
                    start = Some(words.len());
                    words.push(first.to_string());
                }
            }
            words.extend(line_words.map(str::to_string));
        }
        if words.is_empty() {
            return None;
        }
        let index = start.unwrap_or(words.len() - 1);
        let mut speed = Self {
            words,
            index,
            wpm: clamp_wpm(wpm),
            playing: true,
            next_due: now,
        };
        speed.next_due = now + speed.interval();
        Some(speed)
    }

    fn interval(&self) -> Duration {
        Duration::from_millis((60_000 / u64::from(self.wpm)).max(1))
    }

    /// Move to the next word when its time has come. Catches up when frames
    /// were missed, and stops on the last word.
    pub fn advance(&mut self, now: Instant) {
        if !self.playing {
            return;
        }
        let interval = self.interval();
        while now >= self.next_due {
            if self.index + 1 >= self.words.len() {
                self.playing = false;
                break;
            }
            self.index += 1;
            self.next_due += interval;
        }
    }

    /// Pause, resume, or restart after the last word.
    pub fn toggle_play(&mut self, now: Instant) {
        if self.playing {
            self.playing = false;
            return;
        }
        if self.index + 1 >= self.words.len() {
            self.index = 0;
        }
        self.playing = true;
        self.next_due = now + self.interval();
    }

    /// Move one word and pause. Stepping past either end stays put.
    pub fn step(&mut self, delta: isize) {
        self.playing = false;
        let last = self.words.len().saturating_sub(1) as isize;
        self.index = (self.index as isize + delta).clamp(0, last) as usize;
    }

    pub fn set_wpm(&mut self, wpm: u16, now: Instant) {
        self.wpm = clamp_wpm(wpm);
        if self.playing {
            self.next_due = now + self.interval();
        }
    }

    pub fn next_due(&self) -> Instant {
        self.next_due
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    pub fn wpm(&self) -> u16 {
        self.wpm
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn total(&self) -> usize {
        self.words.len()
    }

    pub fn current(&self) -> &str {
        self.words
            .get(self.index)
            .map(String::as_str)
            .unwrap_or_default()
    }

    /// Reading position as a fraction, used to move the article view when
    /// leaving speed reading.
    pub fn ratio(&self) -> f32 {
        if self.words.len() <= 1 {
            0.0
        } else {
            self.index as f32 / (self.words.len() - 1) as f32
        }
    }
}

fn clamp_wpm(wpm: u16) -> u16 {
    wpm.clamp(MIN_WPM, MAX_WPM)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::article::BlockKind;

    fn lines(texts: &[&str]) -> Vec<WrappedLine> {
        texts
            .iter()
            .map(|text| WrappedLine {
                text: (*text).to_string(),
                kind: Some(BlockKind::Paragraph),
                image: None,
                block: None,
            })
            .collect()
    }

    #[test]
    fn starts_at_the_visible_line() {
        let lines = lines(&["one two", "three four", "five"]);
        let speed = SpeedReader::new(&lines, 1, 300).unwrap();
        assert_eq!(speed.total(), 5);
        assert_eq!(speed.current(), "three");

        let speed = SpeedReader::new(&lines, 99, 300).unwrap();
        assert_eq!(speed.current(), "five");

        let speed = SpeedReader::new(&lines, 0, 300).unwrap();
        assert_eq!(speed.current(), "one");
    }

    #[test]
    fn advances_at_the_configured_pace() {
        let lines = lines(&["one two three four"]);
        let start = Instant::now();
        let mut speed = SpeedReader::new_at(&lines, 0, 300, start).unwrap();
        assert!(speed.playing());
        // 300 wpm is one word every 200 ms.
        speed.advance(start + Duration::from_millis(199));
        assert_eq!(speed.current(), "one");
        speed.advance(start + Duration::from_millis(201));
        assert_eq!(speed.current(), "two");
        speed.advance(start + Duration::from_millis(650));
        assert_eq!(speed.current(), "four");
        speed.advance(start + Duration::from_millis(900));
        assert_eq!(speed.current(), "four");
        assert!(!speed.playing());
    }

    #[test]
    fn pauses_resumes_and_restarts() {
        let lines = lines(&["one two"]);
        let start = Instant::now();
        let mut speed = SpeedReader::new_at(&lines, 0, 300, start).unwrap();
        speed.advance(start + Duration::from_millis(201));
        assert_eq!(speed.current(), "two");

        let later = start + Duration::from_secs(10);
        speed.toggle_play(later);
        assert!(!speed.playing());
        speed.advance(later + Duration::from_secs(5));
        assert_eq!(speed.current(), "two");

        // At the last word, resuming starts over.
        speed.toggle_play(later + Duration::from_secs(6));
        assert!(speed.playing());
        assert_eq!(speed.current(), "one");
    }

    #[test]
    fn clamps_and_applies_pace_changes() {
        let lines = lines(&["one two three"]);
        let start = Instant::now();
        let mut speed = SpeedReader::new_at(&lines, 0, 300, start).unwrap();
        speed.set_wpm(10, start);
        assert_eq!(speed.wpm(), MIN_WPM);
        speed.set_wpm(5000, start);
        assert_eq!(speed.wpm(), MAX_WPM);

        speed.set_wpm(600, start);
        speed.advance(start + Duration::from_millis(99));
        assert_eq!(speed.current(), "one");
        speed.advance(start + Duration::from_millis(101));
        assert_eq!(speed.current(), "two");
    }

    #[test]
    fn skips_image_placeholders_and_rows() {
        let mut placeholders = lines(&["one", "[image: alt]", "two"]);
        placeholders[1].kind = Some(BlockKind::Image);
        let speed = SpeedReader::new(&placeholders, 0, 300).unwrap();
        assert_eq!(speed.total(), 2);
        assert_eq!(speed.current(), "one");

        let mut rendered = lines(&["one", "    ", "two"]);
        rendered[1].kind = Some(BlockKind::Image);
        rendered[1].image = Some(Vec::new());
        let speed = SpeedReader::new(&rendered, 1, 300).unwrap();
        assert_eq!(speed.total(), 2);
        assert_eq!(speed.current(), "two");
    }

    #[test]
    fn steps_manually_and_pauses() {
        let lines = lines(&["one two three"]);
        let start = Instant::now();
        let mut speed = SpeedReader::new_at(&lines, 0, 300, start).unwrap();
        assert!(speed.playing());

        speed.step(1);
        assert!(!speed.playing());
        assert_eq!(speed.current(), "two");
        speed.step(1);
        assert_eq!(speed.current(), "three");
        speed.step(1);
        assert_eq!(speed.current(), "three");
        speed.step(-2);
        assert_eq!(speed.current(), "one");
        speed.step(-1);
        assert_eq!(speed.current(), "one");
    }

    #[test]
    fn reports_a_position_ratio() {
        let lines = lines(&["one two three"]);
        let start = Instant::now();
        let mut speed = SpeedReader::new_at(&lines, 0, 300, start).unwrap();
        assert_eq!(speed.ratio(), 0.0);
        speed.advance(start + Duration::from_millis(401));
        assert_eq!(speed.current(), "three");
        assert_eq!(speed.ratio(), 1.0);
    }
}
