use super::*;
use crate::audio::device_detection::InputDeviceKind;
use crate::audio::live_flush::LIVE_CUT_CHUNK_ID;

/// The speech-like tone the VAD tests use, at `t` seconds. Through the 48 kHz
/// path (resampling, echo canceller) Silero keeps hearing it as one utterance
/// only when its first 2 s repeat.
fn speech_like(t: f32) -> f32 {
    let time = t % 2.0;
    let freq1 = 200.0 + (time * 50.0).sin() * 100.0;
    let amplitude = 0.3 + 0.1 * (time * 5.0).sin();
    amplitude
        * (0.5 * (2.0 * std::f32::consts::PI * freq1 * time).sin()
            + 0.3 * (2.0 * std::f32::consts::PI * freq1 * 2.0 * time).sin()
            + 0.2 * (2.0 * std::f32::consts::PI * freq1 * 3.0 * time).sin())
}

fn cut_signal() -> AudioChunk {
    AudioChunk {
        data: Vec::new(),
        sample_rate: 16000,
        timestamp: 0.0,
        chunk_id: LIVE_CUT_CHUNK_ID,
        device_type: DeviceType::Microphone,
    }
}

/// A question mid-recording: the microphone's speech in progress reaches
/// recognition at once, the recording carries on, and the rest of that
/// speech follows on from the cut — nothing lost, nothing sent twice.
#[tokio::test]
async fn a_question_cuts_speech_in_progress_without_stopping_the_recording() {
    let state = RecordingState::new();
    state.start_recording().expect("recording state");
    let (audio_sender, audio_receiver) = mpsc::unbounded_channel::<AudioChunk>();
    let (transcription_sender, mut transcription_receiver) = mpsc::unbounded_channel();
    let sample_rate = 48_000u32;
    let pipeline = AudioPipeline::new(
        audio_receiver,
        transcription_sender,
        state.clone(),
        0,
        sample_rate,
        "Test microphone".to_string(),
        InputDeviceKind::Unknown,
        "Test system audio".to_string(),
        InputDeviceKind::Unknown,
    );
    let running = tokio::spawn(async move { pipeline.run().await });

    // `seconds` of talk on the microphone (silence on the system side) from
    // block `from`, in the 10 ms blocks a capture callback hands over.
    let block = sample_rate as usize / 100;
    let send = |from: usize, seconds: usize, talk: bool| {
        for index in from..from + seconds * 100 {
            let mic: Vec<f32> = (0..block)
                .map(|i| {
                    let t = (index * block + i) as f32 / sample_rate as f32;
                    if talk { speech_like(t) } else { 0.0 }
                })
                .collect();
            let clock = (index + 1) as f64 / 100.0;
            for (data, device_type) in [(mic, DeviceType::Microphone), (vec![0.0; block], DeviceType::System)] {
                audio_sender
                    .send(AudioChunk { data, sample_rate, timestamp: clock, chunk_id: index as u64, device_type })
                    .expect("pipeline running");
            }
        }
    };

    send(0, 4, true);
    audio_sender.send(cut_signal()).expect("pipeline running");
    let first = tokio::time::timeout(std::time::Duration::from_secs(10), transcription_receiver.recv())
        .await
        .expect("the cut piece arrives without waiting for a pause")
        .expect("pipeline still sending");
    assert!(matches!(first.device_type, DeviceType::Microphone));
    let first_end = first.timestamp + first.data.len() as f64 / 16000.0;
    assert!(first.data.len() >= 16000 * 2, "seconds of talk, not a scrap: {} samples from {:.3}s", first.data.len(), first.timestamp);
    assert!(first_end > 3.3 && first_end <= 4.05, "cut near the question: ends at {first_end:.3}s");
    assert!(!running.is_finished(), "the recording carries on");

    // The talk goes on, then a pause closes it.
    send(400, 2, true);
    send(600, 2, false);
    drop(audio_sender);
    running.await.expect("pipeline task").expect("pipeline run");

    let mut rest = Vec::new();
    while let Ok(chunk) = transcription_receiver.try_recv() {
        rest.push(chunk);
    }
    let second = rest
        .iter()
        .find(|chunk| matches!(chunk.device_type, DeviceType::Microphone))
        .expect("the rest of the talk is sent too");
    assert!(
        (second.timestamp - first_end).abs() < 1e-6,
        "the rest starts at the cut: {:.4}s vs {:.4}s",
        second.timestamp,
        first_end
    );
    assert!(second.chunk_id == first.chunk_id + 1, "ids stay consecutive");
    let second_end = second.timestamp + second.data.len() as f64 / 16000.0;
    assert!(second_end > 5.9, "and runs to the end of the talk: {second_end:.3}s");
}
