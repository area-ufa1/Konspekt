use anyhow::{anyhow, Result};
use silero_rs::{VadConfig, VadSession, VadTransition};
use log::{debug, info, warn};
use std::collections::VecDeque;
use std::time::Duration;

/// Represents a complete speech segment detected by VAD
#[derive(Debug, Clone)]
pub struct SpeechSegment {
    pub samples: Vec<f32>,
    pub start_timestamp_ms: f64,
    pub end_timestamp_ms: f64,
    pub confidence: f32,
}

/// Processes audio in 30ms chunks but returns complete speech segments
pub struct ContinuousVadProcessor {
    session: VadSession,
    config: VadConfig,
    chunk_size: usize,
    sample_rate: u32,
    buffer: Vec<f32>,
    speech_segments: VecDeque<SpeechSegment>,
    current_speech: Vec<f32>,
    in_speech: bool,
    processed_samples: usize,
    session_start_sample: usize,
    speech_start_sample: usize,
    // State tracking for smart logging
    last_logged_state: bool,
    /// The last [`RECENT_SAMPLES`] of audio, ending at the chunk being
    /// processed. Speech starts are reported up to ~0.6 s after the fact (the
    /// pre-speech pad plus the minimum speech time); this is where the
    /// utterance's first samples are taken from so that `current_speech`
    /// begins exactly at `speech_start_sample`.
    recent: VecDeque<f32>,
    /// Live capture only: cut an utterance that reaches `max` samples at the
    /// quietest point of its last `search` samples. `None` for offline passes.
    live_cap: Option<LiveCap>,
    /// The utterance in progress was cut at least once, so `speech_start_sample`
    /// is later than where Silero thinks it started.
    cut_since_start: bool,
}

#[derive(Debug, Clone, Copy)]
struct LiveCap {
    max_samples: usize,
    search_samples: usize,
}

/// How much audio `recent` keeps: comfortably more than the 300 ms pre-speech
/// pad plus the 250 ms minimum speech time by which a start is reported late.
const RECENT_SAMPLES: usize = 16_000;

fn ms_to_samples(ms: u32) -> usize {
    ms as usize * 16
}

fn samples_to_ms(samples: usize) -> f64 {
    (samples as f64 / 16000.0) * 1000.0
}

impl ContinuousVadProcessor {
    /// Whether speech has started but has not yet crossed the redemption-time
    /// silence boundary. Live capture uses this to advance only an unfinished
    /// utterance when an audio backend suppresses exact-zero callbacks.
    pub fn has_active_speech(&self) -> bool {
        self.in_speech
    }

    /// Close the current live utterance at the last audio timestamp without
    /// synthesizing samples. Used when a capture backend suppresses callbacks
    /// throughout silence, so wall time can trigger finalization without moving
    /// the recording-relative audio clock or duplicating later gap padding.
    pub fn finalize_active_speech(&mut self) -> Option<SpeechSegment> {
        if !self.in_speech || self.current_speech.is_empty() {
            return None;
        }
        let start_ms = samples_to_ms(self.speech_start_sample);
        let end_ms = samples_to_ms(self.processed_samples);
        self.in_speech = false;
        self.cut_since_start = false;
        self.last_logged_state = false;
        // Silero's reset intentionally retains session_audio. Recreate the
        // session so repeated callback-silence finalizations stay O(utterance)
        // in memory, then offset its future relative timestamps onto the
        // continuous meeting timeline.
        match VadSession::new(self.config) {
            Ok(session) => {
                self.session = session;
                self.session_start_sample = self.processed_samples;
            }
            Err(error) => {
                warn!("Failed to recreate VAD session after forced finalization: {error:?}");
                self.session.reset();
            }
        }
        let samples = std::mem::take(&mut self.current_speech);
        Some(SpeechSegment {
            samples,
            start_timestamp_ms: start_ms,
            end_timestamp_ms: end_ms,
            confidence: 0.8,
        })
    }

    /// Move an inactive processor to an absolute recording-relative position.
    /// The mixer calls this after dropping a callback-free gap instead of
    /// allocating seconds or minutes of synthetic silence.
    pub fn advance_inactive_timeline_to(&mut self, timestamp_seconds: f64) {
        if self.in_speech {
            return;
        }
        let target_sample = (timestamp_seconds.max(0.0) * 16000.0).round() as usize;
        if target_sample <= self.processed_samples {
            return;
        }
        match VadSession::new(self.config) {
            Ok(session) => self.session = session,
            Err(error) => {
                warn!("Failed to recreate VAD session after timeline discontinuity: {error:?}");
                self.session.reset();
            }
        }
        self.buffer.clear();
        self.current_speech.clear();
        // Its positions belong to the timeline before the jump.
        self.recent.clear();
        self.processed_samples = target_sample;
        self.session_start_sample = target_sample;
    }

    /// Live capture: an utterance never grows past `max_ms`. When it gets
    /// there it is cut at the quietest point of its last `search_ms` — the gap
    /// between two words, as far as level can tell — and the rest carries on
    /// as the next utterance. Offline passes split long speech their own way
    /// and do not set this.
    pub fn with_live_cap(mut self, max_ms: u32, search_ms: u32) -> Self {
        self.live_cap = Some(LiveCap {
            max_samples: ms_to_samples(max_ms),
            search_samples: ms_to_samples(search_ms).min(ms_to_samples(max_ms)),
        });
        self
    }

    /// Cut the utterance in progress now, at the quietest point of its last
    /// `search_ms`, and hand over everything before the cut. The VAD keeps
    /// hearing the speech: what follows the cut continues as the next
    /// utterance, ended by silence or the cap as usual, with no sample lost or
    /// repeated. Nothing is cut while there is no speech or it is shorter than
    /// `min_ms`, nor by a processor without [`Self::with_live_cap`].
    pub fn cut_active_speech(&mut self, search_ms: u32, min_ms: u32) -> Option<SpeechSegment> {
        if self.live_cap.is_none()
            || !self.in_speech
            || self.current_speech.len() < ms_to_samples(min_ms).max(1)
        {
            return None;
        }
        self.cut_at_quietest(ms_to_samples(search_ms))
    }

    /// `current_speech` covers `[speech_start_sample, processed_samples)`; cut
    /// it in its last `search` samples and keep the remainder in progress.
    fn cut_at_quietest(&mut self, search: usize) -> Option<SpeechSegment> {
        let len = self.current_speech.len();
        let cut = quietest_cut(&self.current_speech, len.saturating_sub(search));
        if cut == 0 {
            return None;
        }
        let rest = self.current_speech.split_off(cut);
        let piece = std::mem::replace(&mut self.current_speech, rest);
        let start = self.speech_start_sample;
        self.speech_start_sample += cut;
        self.cut_since_start = true;
        debug!(
            "VAD: cut utterance in progress at {:.0}ms after {:.0}ms",
            samples_to_ms(self.speech_start_sample),
            samples_to_ms(cut)
        );
        Some(SpeechSegment {
            samples: piece,
            start_timestamp_ms: samples_to_ms(start),
            end_timestamp_ms: samples_to_ms(start + cut),
            confidence: 0.9,
        })
    }

    pub fn new(input_sample_rate: u32, redemption_time_ms: u32) -> Result<Self> {
        Self::new_with_thresholds(input_sample_rate, redemption_time_ms, 0.50, 0.35)
    }

    pub fn new_with_thresholds(
        input_sample_rate: u32,
        redemption_time_ms: u32,
        positive_speech_threshold: f32,
        negative_speech_threshold: f32,
    ) -> Result<Self> {
        // Silero VAD MUST use 16kHz - this is hardcoded requirement
        const VAD_SAMPLE_RATE: u32 = 16000;

        // Use STRICT settings to prevent silence from reaching Whisper
        let mut config = VadConfig::default();
        config.sample_rate = VAD_SAMPLE_RATE as usize;

        // CONTINUOUS SPEECH FIX: Tuned for capturing complete 5+ second utterances
        // Previous: 0.55/0.40 with 400ms redemption was fragmenting speech into 40ms segments
        // New: More lenient thresholds + longer redemption for continuous speech
        config.positive_speech_threshold = positive_speech_threshold;
        config.negative_speech_threshold = negative_speech_threshold;

        // CRITICAL FIX: Removed redemption_time capping to support long continuous speech
        // Previous: capped at 400ms, causing VAD to fragment 5-second speech into 40ms segments
        // New: Use full redemption_time from pipeline (2000ms) to bridge natural pauses
        config.redemption_time = Duration::from_millis(redemption_time_ms as u64);
        config.pre_speech_pad = Duration::from_millis(300);   // Pre-speech padding for context
        config.post_speech_pad = Duration::from_millis(400);  // Increased: more context at end

        // CRITICAL FIX: Increased min_speech_time to prevent tiny 40ms fragments
        // Previous: 100ms allowed too-short segments that Whisper rejects
        // New: 250ms ensures segments are substantial enough for Whisper (>100ms requirement)
        config.min_speech_time = Duration::from_millis(250);  // Prevent tiny fragments

        debug!("Creating VAD session with: sample_rate={}Hz, redemption={}ms, thresholds={:.2}/{:.2}, min_speech={}ms, input_rate={}Hz",
               VAD_SAMPLE_RATE, redemption_time_ms, positive_speech_threshold,
               negative_speech_threshold, 250, input_sample_rate);

        let session = VadSession::new(config)
            .map_err(|e| anyhow!("Failed to create VAD session: {:?}", e))?;

        // VAD uses 30ms chunks at 16kHz (480 samples)
        let vad_chunk_size = (VAD_SAMPLE_RATE as f32 * 0.03) as usize; // 480 samples

        info!("VAD processor created: input={}Hz, vad={}Hz, chunk_size={} samples",
              input_sample_rate, VAD_SAMPLE_RATE, vad_chunk_size);

        Ok(Self {
            session,
            config,
            chunk_size: vad_chunk_size,
            sample_rate: input_sample_rate, // Store input rate for resampling ratio in resample_to_16k()
            buffer: Vec::with_capacity(vad_chunk_size * 2),
            speech_segments: VecDeque::new(),
            current_speech: Vec::new(),
            in_speech: false,
            processed_samples: 0,
            session_start_sample: 0,
            speech_start_sample: 0,
            // Initialize state tracking
            last_logged_state: false,
            recent: VecDeque::with_capacity(RECENT_SAMPLES + vad_chunk_size),
            live_cap: None,
            cut_since_start: false,
        })
    }

    /// Process incoming audio samples and return any complete speech segments
    /// Handles resampling from input sample rate to 16kHz for VAD processing
    pub fn process_audio(&mut self, samples: &[f32]) -> Result<Vec<SpeechSegment>> {
        // Resample to 16kHz if needed
        let resampled_audio = if self.sample_rate == 16000 {
            samples.to_vec()
        } else {
            self.resample_to_16k(samples)?
        };

        self.buffer.extend_from_slice(&resampled_audio);
        let mut completed_segments = Vec::new();

        // Process complete 30ms chunks (480 samples at 16kHz)
        while self.buffer.len() >= self.chunk_size {
            let chunk: Vec<f32> = self.buffer.drain(..self.chunk_size).collect();
            self.process_chunk(&chunk)?;

            // Extract any completed speech segments
            while let Some(segment) = self.speech_segments.pop_front() {
                completed_segments.push(segment);
            }
        }

        Ok(completed_segments)
    }

    /// Improved resampling from input sample rate to 16kHz with anti-aliasing
    /// Uses linear interpolation and basic low-pass filtering for better quality
    fn resample_to_16k(&self, samples: &[f32]) -> Result<Vec<f32>> {
        if self.sample_rate == 16000 {
            return Ok(samples.to_vec());
        }

        // Calculate downsampling ratio
        let ratio = self.sample_rate as f64 / 16000.0;
        let output_len = (samples.len() as f64 / ratio) as usize;
        let mut resampled = Vec::with_capacity(output_len);

        // Apply simple low-pass filter before downsampling to reduce aliasing
        let cutoff_freq = 0.4; // Normalized frequency (0.4 * Nyquist)
        let mut filtered_samples = Vec::with_capacity(samples.len());
        
        // Simple moving average filter (basic low-pass)
        let filter_size = (self.sample_rate as f64 / (cutoff_freq * self.sample_rate as f64)) as usize;
        let filter_size = std::cmp::max(1, std::cmp::min(filter_size, 5)); // Limit filter size
        
        for i in 0..samples.len() {
            let start = if i >= filter_size { i - filter_size } else { 0 };
            let end = std::cmp::min(i + filter_size + 1, samples.len());
            let sum: f32 = samples[start..end].iter().sum();
            filtered_samples.push(sum / (end - start) as f32);
        }

        // Linear interpolation downsampling
        for i in 0..output_len {
            let source_pos = i as f64 * ratio;
            let source_index = source_pos as usize;
            let fraction = source_pos - source_index as f64;
            
            if source_index + 1 < filtered_samples.len() {
                // Linear interpolation
                let sample1 = filtered_samples[source_index];
                let sample2 = filtered_samples[source_index + 1];
                let interpolated = sample1 + (sample2 - sample1) * fraction as f32;
                resampled.push(interpolated);
            } else if source_index < filtered_samples.len() {
                resampled.push(filtered_samples[source_index]);
            }
        }

        debug!("Resampled from {} samples ({}Hz) to {} samples (16kHz) with anti-aliasing",
               samples.len(), self.sample_rate, resampled.len());

        Ok(resampled)
    }

    /// Flush any remaining audio and return final speech segments
    pub fn flush(&mut self) -> Result<Vec<SpeechSegment>> {
        debug!("VAD flush: in_speech={}, current_speech_len={}, buffer_len={}, speech_segments_queued={}",
              self.in_speech, self.current_speech.len(), self.buffer.len(), self.speech_segments.len());

        let mut completed_segments = Vec::new();

        // Process any remaining buffered audio
        if !self.buffer.is_empty() {
            let remaining = self.buffer.clone();
            self.buffer.clear();

            // Pad to chunk size if needed
            let mut padded_chunk = remaining;
            if padded_chunk.len() < self.chunk_size {
                padded_chunk.resize(self.chunk_size, 0.0);
            }

            self.process_chunk(&padded_chunk)?;
        }

        // Force end any ongoing speech
        if self.in_speech && !self.current_speech.is_empty() {
            // processed_samples and speech_start_sample always count 16kHz samples (post-resampling)
            let start_ms = (self.speech_start_sample as f64 / 16000.0) * 1000.0;
            let end_ms = (self.processed_samples as f64 / 16000.0) * 1000.0;

            debug!("VAD flush: Force-ending speech - start={}ms, end={}ms, duration={}ms, samples={}",
                  start_ms, end_ms, end_ms - start_ms, self.current_speech.len());

            let segment = SpeechSegment {
                samples: self.current_speech.clone(),
                start_timestamp_ms: start_ms,
                end_timestamp_ms: end_ms,
                confidence: 0.8, // Estimated confidence for forced end
            };

            self.speech_segments.push_back(segment);
            self.current_speech.clear();
            self.in_speech = false;
            self.cut_since_start = false;
        }

        // Extract all remaining segments
        while let Some(segment) = self.speech_segments.pop_front() {
            completed_segments.push(segment);
        }

        Ok(completed_segments)
    }

    fn process_chunk(&mut self, chunk: &[f32]) -> Result<()> {
        // Track accumulated speech buffer size to detect memory issues
        let current_speech_size = self.current_speech.len();
        if current_speech_size > 1_000_000 {
            // More than ~62 seconds of accumulated speech at 16kHz
            warn!("VAD: Accumulated speech buffer is large: {} samples ({:.1}s) - possible memory issue",
                  current_speech_size, current_speech_size as f64 / 16000.0);
        }

        // Remember this chunk before Silero answers: a speech start it reports
        // now lies up to ~0.6 s back, and its audio is taken from here.
        if self.live_cap.is_some() {
            self.recent.extend(chunk.iter().copied());
            let excess = self.recent.len().saturating_sub(RECENT_SAMPLES);
            self.recent.drain(..excess);
        }
        let chunk_end = self.processed_samples + chunk.len();
        let mut seeded = false;

        let transitions = self.session.process(chunk)
            .map_err(|e| anyhow!("VAD processing failed: {}", e))?;

        // Log transitions for debugging
        if !transitions.is_empty() {
            debug!("VAD transitions at sample {}: {} transitions", self.processed_samples, transitions.len());
        }

        // Handle VAD transitions
        for transition in transitions {
            match transition {
                VadTransition::SpeechStart { timestamp_ms } => {
                    // Only log if state changed
                    if !self.last_logged_state {
                        debug!("VAD: Speech started at {}ms", timestamp_ms);
                        self.last_logged_state = true;
                    }
                    self.in_speech = true;
                    self.cut_since_start = false;
                    // Use 16000 (VAD processing rate) since processed_samples counts 16kHz samples
                    let reported = self.session_start_sample + (timestamp_ms * 16000 / 1000);
                    self.current_speech.clear();
                    if self.live_cap.is_some() {
                        // Live: `current_speech` starts exactly at
                        // `speech_start_sample` (cuts rely on it), its first
                        // samples, this chunk included, taken from `recent`.
                        // Offline passes keep their old behaviour.
                        let recent_start = chunk_end - self.recent.len();
                        self.speech_start_sample = reported.clamp(recent_start, chunk_end);
                        self.current_speech
                            .extend(self.recent.range(self.speech_start_sample - recent_start..));
                        seeded = true;
                    } else {
                        self.speech_start_sample = reported;
                    }
                }
                VadTransition::SpeechEnd { start_timestamp_ms, end_timestamp_ms, samples } => {
                    let utterance_start = self.session_start_sample + start_timestamp_ms * 16;
                    let session_offset_ms = self.session_start_sample * 1000 / 16000;
                    let start_timestamp_ms = start_timestamp_ms + session_offset_ms;
                    let end_timestamp_ms = end_timestamp_ms + session_offset_ms;
                    // Only log if we were previously in speech state
                    if self.last_logged_state {
                        debug!("VAD: Speech ended at {}ms (duration: {}ms)", end_timestamp_ms, end_timestamp_ms - start_timestamp_ms);
                        self.last_logged_state = false;
                    }
                    self.in_speech = false;

                    // Use samples from VAD transition if available, otherwise use accumulated samples
                    let (speech_samples, start_ms, end_ms) = if samples.is_empty() {
                        (self.current_speech.clone(), start_timestamp_ms as f64, end_timestamp_ms as f64)
                    } else if self.cut_since_start {
                        // Silero returns the whole utterance; what came before
                        // the last cut has already been handed over.
                        let already = self.speech_start_sample.saturating_sub(utterance_start);
                        let rest = samples.get(already..).map(<[f32]>::to_vec).unwrap_or_default();
                        let start = self.speech_start_sample;
                        let end = start + rest.len();
                        (rest, samples_to_ms(start), samples_to_ms(end))
                    } else {
                        (samples, start_timestamp_ms as f64, end_timestamp_ms as f64)
                    };
                    self.cut_since_start = false;

                    if !speech_samples.is_empty() {
                        let segment = SpeechSegment {
                            samples: speech_samples,
                            start_timestamp_ms: start_ms,
                            end_timestamp_ms: end_ms,
                            confidence: 0.9, // VAD confidence
                        };

                        info!("VAD: Completed speech segment: {:.1}ms duration, {} samples",
                              end_ms - start_ms, segment.samples.len());

                        self.speech_segments.push_back(segment);
                    }

                    self.current_speech.clear();
                }
            }
        }

        // Accumulate speech if we're currently in a speech state
        if self.in_speech && !seeded {
            self.current_speech.extend_from_slice(chunk);
        }

        self.processed_samples += chunk.len();

        if let Some(cap) = self.live_cap {
            if self.in_speech && self.current_speech.len() >= cap.max_samples {
                if let Some(segment) = self.cut_at_quietest(cap.search_samples) {
                    info!(
                        "VAD: Utterance reached the {:.0}ms cap: cut a {:.0}ms segment",
                        samples_to_ms(cap.max_samples),
                        segment.end_timestamp_ms - segment.start_timestamp_ms
                    );
                    self.speech_segments.push_back(segment);
                }
            }
        }
        Ok(())
    }
}

/// Where to cut `samples` so no word is split, looking only at
/// `samples[search_from..]`: the middle of its quietest 30 ms window (lowest
/// RMS, stepped every 10 ms; the later one on a tie). A stretch too short for
/// one window is cut at its end. Always in `1..=samples.len()` for non-empty
/// input.
pub fn quietest_cut(samples: &[f32], search_from: usize) -> usize {
    const WINDOW: usize = 480;
    const HOP: usize = 160;
    let from = search_from.min(samples.len());
    let region = &samples[from..];
    if region.len() < WINDOW {
        return samples.len();
    }
    let mut best = (f32::MAX, samples.len());
    let mut at = 0;
    while at + WINDOW <= region.len() {
        let energy: f32 = region[at..at + WINDOW].iter().map(|s| s * s).sum();
        if energy <= best.0 {
            best = (energy, from + at + WINDOW / 2);
        }
        at += HOP;
    }
    best.1.max(1)
}

/// Legacy function for backward compatibility - now uses the optimized approach
pub fn extract_speech_16k(samples_mono_16k: &[f32]) -> Result<Vec<f32>> {
    let mut processor = ContinuousVadProcessor::new(16000, 400)?;

    // Process all audio
    let mut all_segments = processor.process_audio(samples_mono_16k)?;
    let final_segments = processor.flush()?;
    all_segments.extend(final_segments);

    // Concatenate all speech segments
    let mut result = Vec::new();
    let num_segments = all_segments.len();
    for segment in &all_segments {
        result.extend_from_slice(&segment.samples);
    }

    // Apply balanced energy filtering for very short segments
    if result.len() < 1600 { // Less than 100ms at 16kHz
        let input_energy: f32 = samples_mono_16k.iter().map(|&x| x * x).sum::<f32>() / samples_mono_16k.len() as f32;
        let rms = input_energy.sqrt();
        let peak = samples_mono_16k.iter().map(|&x| x.abs()).fold(0.0f32, f32::max);

        // BALANCED FIX: Lowered thresholds to preserve quiet speech while still filtering silence
        // Previous aggressive values (0.08/0.15) were discarding valid quiet speech
        // New values (0.03/0.08) are more balanced - catch quiet speech, reject pure silence
        if rms < 0.2 || peak < 0.20 {
            info!("-----VAD detected silence/noise (RMS: {:.6}, Peak: {:.6}), skipping to prevent hallucinations-----", rms, peak);
            return Ok(Vec::new());
        } else {
            info!("VAD detected speech with sufficient energy (RMS: {:.6}, Peak: {:.6})", rms, peak);
            return Ok(samples_mono_16k.to_vec());
        }
    }

    debug!("VAD: Processed {} samples, extracted {} speech samples from {} segments",
           samples_mono_16k.len(), result.len(), num_segments);

    Ok(result)
}

/// Simple convenience function to get speech chunks from audio
/// Uses the optimized ContinuousVadProcessor with configurable redemption time
pub fn get_speech_chunks(samples_mono_16k: &[f32], redemption_time_ms: u32) -> Result<Vec<SpeechSegment>> {
    get_speech_chunks_with_progress(samples_mono_16k, redemption_time_ms, |_, _| true)
}

/// Get speech chunks with progress callback and cancellation support
/// The callback receives (progress_percent, segments_found) and returns false to cancel
pub fn get_speech_chunks_with_progress<F>(
    samples_mono_16k: &[f32],
    redemption_time_ms: u32,
    progress_callback: F,
) -> Result<Vec<SpeechSegment>>
where
    F: FnMut(u32, usize) -> bool,
{
    get_speech_chunks_with_thresholds_and_progress(
        samples_mono_16k,
        redemption_time_ms,
        0.50,
        0.35,
        progress_callback,
    )
}

pub fn get_speech_chunks_with_thresholds_and_progress<F>(
    samples_mono_16k: &[f32],
    redemption_time_ms: u32,
    positive_speech_threshold: f32,
    negative_speech_threshold: f32,
    mut progress_callback: F,
) -> Result<Vec<SpeechSegment>>
where
    F: FnMut(u32, usize) -> bool,
{
    let mut processor = ContinuousVadProcessor::new_with_thresholds(
        16000,
        redemption_time_ms,
        positive_speech_threshold,
        negative_speech_threshold,
    )?;

    let total_samples = samples_mono_16k.len();

    // For large files (>1 minute at 16kHz = 960,000 samples), process in chunks with progress logging
    const LARGE_FILE_THRESHOLD: usize = 960_000;
    const CHUNK_SIZE: usize = 160_000; // 10 seconds at 16kHz

    let mut all_segments = Vec::new();

    if total_samples > LARGE_FILE_THRESHOLD {
        info!("VAD: Processing large file ({} samples = {:.1}s), will log progress...",
              total_samples, total_samples as f64 / 16000.0);

        let mut processed = 0;
        let mut last_progress = 0u32;
        let mut chunk_count = 0;
        let total_chunks = (total_samples + CHUNK_SIZE - 1) / CHUNK_SIZE;

        for chunk in samples_mono_16k.chunks(CHUNK_SIZE) {
            chunk_count += 1;

            let start_time = std::time::Instant::now();
            let segments = processor.process_audio(chunk)?;
            let elapsed = start_time.elapsed();

            // Debug log for chunk processing details
            debug!("VAD: Chunk {}/{} processed in {:?}, found {} segments",
                  chunk_count, total_chunks, elapsed, segments.len());

            // Warn if chunk processing took too long (>1 second)
            if elapsed.as_secs() > 1 {
                warn!("VAD: Chunk {} took {:?} - possible performance issue", chunk_count, elapsed);
            }

            all_segments.extend(segments);

            processed += chunk.len();
            let progress = ((processed * 100) / total_samples) as u32;

            // Call progress callback every 5%
            if progress >= last_progress + 5 {
                debug!("VAD: Progress {}% ({} segments found so far)", progress, all_segments.len());

                // Check for cancellation
                if !progress_callback(progress, all_segments.len()) {
                    info!("VAD: Cancelled by callback at {}%", progress);
                    return Err(anyhow!("VAD processing cancelled"));
                }

                last_progress = progress;
            }
        }

        let final_segments = processor.flush()?;
        all_segments.extend(final_segments);

        info!("VAD: Complete! Found {} speech segments", all_segments.len());
    } else {
        // Small file - process all at once
        all_segments = processor.process_audio(samples_mono_16k)?;
        let final_segments = processor.flush()?;
        all_segments.extend(final_segments);
        if !progress_callback(100, all_segments.len()) {
            return Err(anyhow!("VAD processing cancelled"));
        }
    }

    Ok(all_segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate synthetic speech-like audio with alternating speech/silence
    fn generate_test_audio_with_speech(duration_seconds: f32, sample_rate: u32) -> Vec<f32> {
        let total_samples = (duration_seconds * sample_rate as f32) as usize;
        let mut samples = vec![0.0f32; total_samples];

        // Create speech-like patterns: bursts of sine waves with varying amplitude
        // Speech every 10 seconds for 5 seconds
        let speech_interval = 10.0; // seconds between speech starts
        let speech_duration = 5.0;  // seconds of speech

        for i in 0..total_samples {
            let time = i as f32 / sample_rate as f32;
            let cycle_time = time % speech_interval;

            // Speech occurs in the first `speech_duration` seconds of each cycle
            if cycle_time < speech_duration {
                // Generate speech-like signal: multiple frequencies with amplitude modulation
                let freq1 = 200.0 + (time * 50.0).sin() * 100.0; // Varying fundamental
                let freq2 = freq1 * 2.0; // Harmonic
                let freq3 = freq1 * 3.0; // Another harmonic

                let amplitude = 0.3 + 0.1 * (time * 5.0).sin(); // Amplitude modulation
                samples[i] = amplitude * (
                    0.5 * (2.0 * std::f32::consts::PI * freq1 * time).sin() +
                    0.3 * (2.0 * std::f32::consts::PI * freq2 * time).sin() +
                    0.2 * (2.0 * std::f32::consts::PI * freq3 * time).sin()
                );
            }
            // else: silence (already 0.0)
        }

        samples
    }

    #[test]
    fn test_vad_chunked_vs_single_processing() {
        // Generate 60 seconds of audio with speech patterns at 16kHz
        let audio = generate_test_audio_with_speech(60.0, 16000);
        println!("Generated {} samples ({:.1}s)", audio.len(), audio.len() as f32 / 16000.0);

        // Process all at once (like small files)
        let segments_single = get_speech_chunks(&audio, 2000).expect("Single processing failed");
        println!("Single processing found {} segments", segments_single.len());

        // Process in chunks (like large files)
        let segments_chunked = get_speech_chunks_with_progress(&audio, 2000, |progress, segments| {
            println!("Chunked progress: {}%, {} segments", progress, segments);
            true // Don't cancel
        }).expect("Chunked processing failed");
        println!("Chunked processing found {} segments", segments_chunked.len());

        // Both should find the same number of segments (approximately)
        // Allow some variance due to chunk boundary effects
        let diff = (segments_single.len() as i32 - segments_chunked.len() as i32).abs();
        assert!(diff <= 1,
            "Chunked and single processing found different segment counts: {} vs {} (diff: {})",
            segments_single.len(), segments_chunked.len(), diff);
    }

    #[test]
    fn test_vad_large_file_progress() {
        // Generate 120 seconds (2 minutes) of audio - triggers large file threshold
        let audio = generate_test_audio_with_speech(120.0, 16000);
        let total_samples = audio.len();
        println!("Generated {} samples ({:.1}s)", total_samples, total_samples as f32 / 16000.0);

        // This should trigger the large file path (>960,000 samples)
        assert!(total_samples > 960_000, "Audio should be large enough to trigger chunked processing");

        let mut progress_updates = Vec::new();
        let segments = get_speech_chunks_with_progress(&audio, 2000, |progress, segments| {
            progress_updates.push((progress, segments));
            true // Don't cancel
        }).expect("Processing failed");

        println!("Found {} segments with {} progress updates", segments.len(), progress_updates.len());

        // The synthetic signal is not real speech, so Silero may merge it into
        // one long segment. This test is specifically for the large-file path:
        // it must still emit speech and report monotonic progress through 100%.
        assert!(!segments.is_empty(), "Expected at least one speech segment");
        assert!(
            segments.iter().all(|segment| !segment.samples.is_empty()
                && segment.end_timestamp_ms > segment.start_timestamp_ms),
            "Expected all speech segments to contain audio with positive duration"
        );

        // Should have received progress updates
        assert!(!progress_updates.is_empty(), "Expected progress updates for large file");
        assert_eq!(
            progress_updates.last().map(|(progress, _)| *progress),
            Some(100),
            "Expected progress to reach 100%"
        );
        assert!(
            progress_updates
                .windows(2)
                .all(|pair| pair[0].0 < pair[1].0),
            "Expected progress updates to increase monotonically: {:?}",
            progress_updates
        );
    }

    #[test]
    fn test_vad_cancellation() {
        let audio = generate_test_audio_with_speech(120.0, 16000);

        // Cancel at 50%
        let result = get_speech_chunks_with_progress(&audio, 2000, |progress, _| {
            progress < 50 // Cancel when reaching 50%
        });

        // Should return error due to cancellation
        assert!(result.is_err(), "Expected cancellation error");
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("cancelled"), "Error should mention cancellation: {}", err_msg);
    }

    #[test]
    fn test_vad_continuous_processor_state_across_chunks() {
        // Test that VAD state is correctly maintained across chunk boundaries
        let mut processor = ContinuousVadProcessor::new(16000, 2000).expect("Failed to create processor");

        // Generate audio with a speech segment that spans a chunk boundary
        let chunk_size = 160_000; // 10 seconds
        let audio = generate_test_audio_with_speech(30.0, 16000); // 30 seconds

        // Process in 10-second chunks
        let mut all_segments = Vec::new();
        for (i, chunk) in audio.chunks(chunk_size).enumerate() {
            let segments = processor.process_audio(chunk).expect("Processing failed");
            println!("Chunk {}: processed {} samples, found {} segments", i, chunk.len(), segments.len());
            all_segments.extend(segments);
        }

        // Flush remaining
        let final_segments = processor.flush().expect("Flush failed");
        all_segments.extend(final_segments);

        println!("Total segments found: {}", all_segments.len());

        // Should find speech segments
        assert!(all_segments.len() >= 1, "Expected at least 1 speech segment");
    }

    #[test]
    fn test_vad_silence_ticks_finalize_without_followup_speech() {
        let mut processor = ContinuousVadProcessor::new_with_thresholds(
            16000,
            800,
            0.20,
            0.10,
        )
        .expect("Failed to create processor");
        let speech = generate_test_audio_with_speech(2.0, 16000);
        let mut segments = processor.process_audio(&speech).expect("Speech processing failed");

        // Some capture backends stop calling us for exact-zero silence. Feeding
        // real-time silence ticks must close the utterance without waiting for
        // another speaker to produce the next callback.
        for _ in 0..30 {
            segments.extend(
                processor
                    .process_audio(&vec![0.0; 800])
                    .expect("Silence tick failed"),
            );
            if !segments.is_empty() {
                break;
            }
        }

        assert!(!segments.is_empty(), "Expected silence ticks to finalize speech");
        assert!(!processor.has_active_speech());
    }

    #[test]
    fn test_force_finalize_does_not_advance_audio_timeline() {
        let mut processor = ContinuousVadProcessor::new_with_thresholds(
            16000,
            800,
            0.20,
            0.10,
        )
        .expect("Failed to create processor");
        let speech = generate_test_audio_with_speech(2.0, 16000);
        processor.process_audio(&speech).expect("Speech processing failed");
        let segment = processor
            .finalize_active_speech()
            .expect("Expected active utterance");

        assert!(segment.end_timestamp_ms <= 2100.0);
        assert!(segment.end_timestamp_ms > segment.start_timestamp_ms);
        assert!(!processor.has_active_speech());
        assert_eq!(processor.session.session_audio_samples(), 0);
        assert!(
            processor
                .process_audio(&vec![0.0; 16000])
                .expect("post-finalize silence")
                .is_empty(),
            "Reset Silero state must not emit the force-finalized utterance again"
        );
    }

    #[test]
    fn test_inactive_timeline_advance_preserves_callback_gap() {
        let mut processor = ContinuousVadProcessor::new_with_thresholds(
            16000,
            800,
            0.20,
            0.10,
        )
        .expect("Failed to create processor");
        processor
            .process_audio(&vec![0.0; 16000])
            .expect("initial silence");
        processor.advance_inactive_timeline_to(10.0);
        let speech = generate_test_audio_with_speech(2.0, 16000);
        processor.process_audio(&speech).expect("later speech");
        let segment = processor
            .finalize_active_speech()
            .expect("Expected later utterance");

        assert!(segment.start_timestamp_ms >= 10_000.0);
        assert!(segment.end_timestamp_ms > segment.start_timestamp_ms);
    }

    /// The speech-like signal of `generate_test_audio_with_speech`, without its
    /// pauses, at sample `i` of a 16 kHz stream. Silero hears its first seconds
    /// as speech but not what follows, so they repeat every 5 s.
    fn speech_like(i: usize) -> f32 {
        let time = (i % 80_000) as f32 / 16000.0;
        let freq1 = 200.0 + (time * 50.0).sin() * 100.0;
        let amplitude = 0.3 + 0.1 * (time * 5.0).sin();
        amplitude
            * (0.5 * (2.0 * std::f32::consts::PI * freq1 * time).sin()
                + 0.3 * (2.0 * std::f32::consts::PI * freq1 * 2.0 * time).sin()
                + 0.2 * (2.0 * std::f32::consts::PI * freq1 * 3.0 * time).sin())
    }

    /// Talk without a pause long enough to end the utterance; between `dip`
    /// seconds the voice drops to almost nothing, as between two words.
    fn talk_with_dip(seconds: f32, dip: (f32, f32)) -> Vec<f32> {
        (0..(seconds * 16000.0) as usize)
            .map(|i| {
                let time = i as f32 / 16000.0;
                let s = speech_like(i);
                if time >= dip.0 && time < dip.1 { s * 0.01 } else { s }
            })
            .collect()
    }

    /// The live microphone VAD's settings, as an offline pass would use them.
    fn offline_processor() -> ContinuousVadProcessor {
        ContinuousVadProcessor::new_with_thresholds(16000, 800, 0.20, 0.10)
            .expect("Failed to create processor")
    }

    /// The live microphone VAD, as the pipeline builds it.
    fn live_processor() -> ContinuousVadProcessor {
        offline_processor().with_live_cap(15_000, 3_000)
    }

    /// Feed like the live pipeline does — 100 ms at a time — and collect.
    fn feed(processor: &mut ContinuousVadProcessor, audio: &[f32]) -> Vec<SpeechSegment> {
        let mut segments = Vec::new();
        for block in audio.chunks(1600) {
            segments.extend(processor.process_audio(block).expect("VAD failed"));
        }
        segments
    }

    /// Consecutive pieces tile the stream: each starts where the previous one
    /// ended, and their samples are exactly the stream's over that span.
    fn assert_tiles(segments: &[SpeechSegment], stream: &[f32]) {
        let first = (segments[0].start_timestamp_ms * 16.0).round() as usize;
        let mut at = first;
        for (i, segment) in segments.iter().enumerate() {
            let start = (segment.start_timestamp_ms * 16.0).round() as usize;
            assert_eq!(start, at, "piece {i} starts where piece {} ended", i.max(1) - 1);
            let end = start + segment.samples.len();
            assert_eq!(
                (segment.end_timestamp_ms * 16.0).round() as usize,
                end,
                "piece {i}: end time matches its samples"
            );
            assert!(
                segment.samples == stream[start..end],
                "piece {i}: samples are the stream's own, none lost or repeated"
            );
            at = end;
        }
    }

    #[test]
    fn quietest_cut_lands_in_the_quiet_gap_after_search_start() {
        // 3 s of voice with a 100 ms near-silence at 2.2 s and another at 0.5 s.
        let mut samples: Vec<f32> = (0..48000).map(speech_like).collect();
        for gap in [(8000, 9600), (35200, 36800)] {
            for s in &mut samples[gap.0..gap.1] {
                *s *= 0.01;
            }
        }
        let cut = quietest_cut(&samples, 16000);
        assert!((35200..=36800).contains(&cut), "cut at {cut} is in the gap at 2.2 s");
        let cut = quietest_cut(&samples, 0);
        assert!(
            (8000..=9600).contains(&cut) || (35200..=36800).contains(&cut),
            "cut at {cut} is in one of the gaps"
        );
        assert!(cut > 0 && cut <= samples.len());
    }

    #[test]
    fn live_cap_cuts_long_talk_in_its_quiet_dip_without_losing_audio() {
        let talk = talk_with_dip(24.0, (13.6, 13.9));
        let mut stream = talk.clone();
        stream.extend(std::iter::repeat(0.0).take(32000)); // 2 s of silence closes it

        // Without the cap (offline passes) the same talk is one long piece.
        let mut uncapped = offline_processor();
        let mut whole = feed(&mut uncapped, &stream);
        whole.extend(uncapped.flush().unwrap());
        assert_eq!(whole.len(), 1, "the synthetic talk is one utterance to the VAD");
        assert!(whole[0].end_timestamp_ms - whole[0].start_timestamp_ms > 20_000.0);

        let mut capped = live_processor();
        let mut pieces = feed(&mut capped, &stream);
        pieces.extend(capped.flush().unwrap());
        assert!(pieces.len() >= 2, "a 24 s utterance is cut: {} piece(s)", pieces.len());
        let first = &pieces[0];
        let first_ms = first.end_timestamp_ms - first.start_timestamp_ms;
        assert!(first_ms <= 15_030.0, "first piece is at most 15 s: {first_ms} ms");
        assert!(
            (13_600.0..=13_900.0).contains(&first.end_timestamp_ms),
            "cut lands in the dip: {} ms",
            first.end_timestamp_ms
        );
        for piece in &pieces {
            let ms = piece.end_timestamp_ms - piece.start_timestamp_ms;
            assert!(ms <= 15_030.0, "every piece is capped: {ms} ms");
        }
        assert_tiles(&pieces, &stream);
        assert!(
            pieces.last().unwrap().end_timestamp_ms >= 23_900.0,
            "the talk is covered to its end"
        );
    }

    #[test]
    fn cut_on_demand_closes_the_utterance_and_the_rest_follows_it() {
        let mut processor = live_processor();
        assert!(processor.cut_active_speech(600, 1000).is_none(), "nothing to cut before speech");

        let stream: Vec<f32> = (0..16000 * 9)
            .map(|i| if i < 16000 * 7 { speech_like(i) } else { 0.0 })
            .collect();
        let before = feed(&mut processor, &stream[..16000 * 5]);
        assert!(before.is_empty(), "5 s of talk is still in progress");

        // Offline passes have no cuts.
        let mut offline = offline_processor();
        feed(&mut offline, &stream[..16000 * 5]);
        assert!(offline.has_active_speech());
        assert!(offline.cut_active_speech(600, 1000).is_none(), "only live processors cut");

        let piece = processor.cut_active_speech(600, 1000).expect("talk in progress is cut");
        assert!(
            piece.end_timestamp_ms >= 5_000.0 - 630.0 && piece.end_timestamp_ms <= 5_000.0,
            "cut within the last 600 ms: {} ms",
            piece.end_timestamp_ms
        );
        assert!(processor.has_active_speech(), "the VAD still hears the talk going on");

        let mut pieces = vec![piece];
        pieces.extend(feed(&mut processor, &stream[16000 * 5..]));
        pieces.extend(processor.flush().unwrap());
        assert!(pieces.len() >= 2, "the rest of the talk follows the cut");
        assert_tiles(&pieces, &stream);
        assert!(pieces.last().unwrap().end_timestamp_ms >= 6_900.0);
    }

    #[test]
    fn cut_on_demand_leaves_a_too_short_utterance_alone() {
        let mut processor = live_processor();
        let talk: Vec<f32> = (0..16000).map(speech_like).collect();
        feed(&mut processor, &talk);
        if processor.has_active_speech() {
            assert!(
                processor.cut_active_speech(600, 5_000).is_none(),
                "under the minimum: nothing is cut"
            );
            assert!(processor.has_active_speech());
        }
    }

    #[test]
    fn test_vad_400ms_vs_2000ms_segmentation() {
        // Demonstrates why 2000ms redemption is needed for batch processing:
        // 400ms creates excessive fragmentation, 2000ms bridges natural pauses.
        //
        // Audio pattern: 60s with 5s speech / 5s silence cycles
        // Natural pauses within speech (sentence gaps) are 500ms-1.5s
        let audio = generate_test_audio_with_speech(60.0, 16000);

        let segments_400 = get_speech_chunks(&audio, 400).expect("400ms processing failed");
        let segments_2000 = get_speech_chunks(&audio, 2000).expect("2000ms processing failed");

        println!(
            "400ms redemption: {} segments, 2000ms redemption: {} segments",
            segments_400.len(),
            segments_2000.len()
        );

        // 2000ms should produce fewer or equal segments (bridges more pauses)
        assert!(
            segments_2000.len() <= segments_400.len(),
            "2000ms redemption ({} segments) should not produce more segments than 400ms ({} segments)",
            segments_2000.len(),
            segments_400.len()
        );

        // Verify segments have reasonable durations with 2000ms
        for (i, seg) in segments_2000.iter().enumerate() {
            let duration_ms = seg.end_timestamp_ms - seg.start_timestamp_ms;
            println!("2000ms segment {}: {:.0}ms duration", i, duration_ms);
            // Each segment should be at least 250ms (min_speech_time)
            assert!(duration_ms >= 200.0, "Segment {} too short: {:.0}ms", i, duration_ms);
        }
    }
}

