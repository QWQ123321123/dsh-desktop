//! Windows-native speech-to-text for the chat composer (按住说话).
//!
//! Uses WinRT `Windows.Media.SpeechRecognition` with the system dictation
//! grammar — offline, no API key, recognition quality follows the system
//! language pack. The shell only relays: the control channel starts/stops a
//! continuous session, and the injected page script inserts the final text
//! into the dsh composer (a React controlled textarea).
//!
//! One session at a time, owned by a dedicated worker thread. The worker
//! initializes COM itself (handler threads from the HTTP pool have no
//! apartment), creates the recognizer, and parks until `/speech/stop` flips
//! the flag — so the recognizer's lifecycle never depends on which thread
//! happens to call in, and a new `/speech/start` always tears down any live
//! session first (page reloads and lost mouseups must not leak a microphone).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::Foundation::TypedEventHandler;
use windows::Media::Devices::{AudioDeviceRole, MediaDevice};
use windows::Media::SpeechRecognition::{
    SpeechContinuousRecognitionResultGeneratedEventArgs, SpeechContinuousRecognitionSession,
    SpeechRecognitionResultStatus, SpeechRecognizer,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

struct SpeechSession {
    stop: Arc<AtomicBool>,
    results: Arc<Mutex<String>>,
    done: Option<mpsc::Receiver<()>>,
}

static SPEECH: Mutex<Option<SpeechSession>> = Mutex::new(None);

/// Runs on the worker thread after COM is initialized. Sends the outcome of
/// the start attempt on `started` (Ok once recognition is live), then parks
/// until the stop flag, then stops the session cleanly on the same thread
/// that created it.
fn run_session(
    stop: &AtomicBool,
    finalized: Arc<AtomicBool>,
    results: Arc<Mutex<String>>,
    started: &mpsc::Sender<Result<(), String>>,
) {
    let bail = |msg: String| {
        let _ = started.send(Err(msg));
    };
    let recognizer = match SpeechRecognizer::new() {
        Ok(r) => r,
        Err(e) => return bail(format!("无法创建语音识别器（检查系统设置中是否允许应用使用麦克风）：{e}")),
    };
    let session = match recognizer.ContinuousRecognitionSession() {
        Ok(s) => s,
        Err(e) => return bail(format!("无法初始化连续识别：{e}")),
    };
    let results_for_event = results.clone();
    let finalized_for_event = finalized.clone();
    let handler = TypedEventHandler::<
        SpeechContinuousRecognitionSession,
        SpeechContinuousRecognitionResultGeneratedEventArgs,
    >::new(move |_, args| {
        if let Some(args) = args.as_ref() {
            if let Ok(result) = args.Result() {
                // 只保留最后一次非空文本：dictation 的中间结果经常是空串，
                // 若把每次事件都写进去，最后一次空事件会把已识别内容冲掉，
                // /speech/stop 就只会返回空文本。
                if let Ok(text) = result.Text() {
                    if !text.is_empty() {
                        *results_for_event.lock().expect("speech results") = text.to_string();
                    }
                }
                // 最终结果（说完后停顿片刻定稿）到达即通知主循环，stop 的
                // 定稿宽限可以提前结束，不用白等。
                if let Ok(status) = result.Status() {
                    if status == SpeechRecognitionResultStatus::Success {
                        finalized_for_event.store(true, Ordering::Relaxed);
                    }
                }
            }
        }
        Ok(())
    });
    let token = match session.ResultGenerated(&handler) {
        Ok(t) => t,
        Err(e) => return bail(format!("注册识别事件失败：{e}")),
    };
    let compile = match recognizer.CompileConstraintsAsync().and_then(|op| op.get()) {
        Ok(c) => c,
        Err(e) => {
            let _ = session.RemoveResultGenerated(token);
            return bail(format!("编译语音识别约束失败：{e}"));
        }
    };
    if !matches!(compile.Status(), Ok(SpeechRecognitionResultStatus::Success)) {
        let _ = session.RemoveResultGenerated(token);
        return bail("语音识别约束编译未通过".to_string());
    }
    if let Err(e) = session.StartAsync().and_then(|op| op.get()) {
        let _ = session.RemoveResultGenerated(token);
        return bail(format!("启动识别失败（检查系统设置中是否允许应用使用麦克风）：{e}"));
    }
    let _ = started.send(Ok(()));

    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(50));
    }
    // 定稿宽限：松开瞬间最后一句通常还是中间结果，引擎需要约半秒静音
    // 才会产出最终结果；等到 Success 事件或 700ms 超时再停，避免丢尾。
    let deadline = Instant::now() + Duration::from_millis(700);
    while !finalized.load(Ordering::Relaxed) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(30));
    }
    let _ = session.StopAsync().and_then(|op| op.get());
    let _ = session.RemoveResultGenerated(token);
}

fn speech_thread(
    stop: Arc<AtomicBool>,
    finalized: Arc<AtomicBool>,
    results: Arc<Mutex<String>>,
    started: mpsc::Sender<Result<(), String>>,
) {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if com.is_ok() {
        run_session(&stop, finalized, results, &started);
    } else {
        let _ = started.send(Err(format!("COM 初始化失败：{com}")));
    }
    unsafe { CoUninitialize() };
}

/// Start listening. Idempotent from the caller's view: any prior session is
/// stopped first, so a new hold-to-talk press always starts clean.
pub fn start() -> String {
    let mut guard = SPEECH.lock().expect("speech session");
    let old = guard.take();
    drop(guard);
    if let Some(old) = old {
        old.stop.store(true, Ordering::Relaxed);
        await_teardown(old, Duration::from_secs(6));
    }
    let stop = Arc::new(AtomicBool::new(false));
    let finalized = Arc::new(AtomicBool::new(false));
    let results = Arc::new(Mutex::new(String::new()));
    let (tx, rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let join = thread::spawn({
        let stop = stop.clone();
        let finalized = finalized.clone();
        let results = results.clone();
        move || {
            speech_thread(stop, finalized, results, tx);
            let _ = done_tx.send(());
        }
    });
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {
            *SPEECH.lock().expect("speech session") = Some(SpeechSession {
                stop,
                results,
                done: Some(done_rx),
            });
            let _ = join; // detached; the done channel tracks teardown
            // WinRT 识别器固定使用系统的“默认通信设备”麦克风。多数机器上
            // 通信默认与用户平时选择的默认输入设备不同（比如蓝牙耳机只在
            // 通话模式下才启用麦克风），提前告知前端，前端可以提示用户。
            let comm = MediaDevice::GetDefaultAudioCaptureId(AudioDeviceRole::Communications);
            let user = MediaDevice::GetDefaultAudioCaptureId(AudioDeviceRole::Default);
            let mismatch = match (&comm, &user) {
                (Ok(c), Ok(u)) => {
                    let c = c.to_string();
                    let u = u.to_string();
                    !c.eq_ignore_ascii_case(&u)
                }
                _ => false,
            };
            serde_json::json!({"ok": true, "mic_mismatch": mismatch}).to_string()
        }
        Ok(Err(msg)) => {
            let _ = join.join();
            serde_json::json!({"ok": false, "error": msg}).to_string()
        }
        Err(_) => {
            // The worker is wedged (start never completed). Flag it so it
            // exits if it ever unblocks.
            stop.store(true, Ordering::Relaxed);
            *SPEECH.lock().expect("speech session") = Some(SpeechSession {
                stop,
                results,
                done: Some(done_rx),
            });
            let _ = join;
            "{\"ok\":false,\"error\":\"语音识别启动超时\"}".into()
        }
    }
}

/// Wait (bounded) for the worker thread to finish tearing the session down.
/// A wedged StopAsync must never block the control channel, so on timeout the
/// worker is simply abandoned — the recognizer is left to clean itself up.
fn await_teardown(mut s: SpeechSession, timeout: Duration) {
    if let Some(done) = s.done.take() {
        let _ = done.recv_timeout(timeout);
    }
}

/// Stop listening and return the accumulated text (may be empty if nothing
/// was recognized, e.g. the press was too short or speech never started).
pub fn stop() -> String {
    let mut guard = SPEECH.lock().expect("speech session");
    let s = guard.take();
    drop(guard);
    let Some(mut s) = s else {
        return "{\"ok\":true,\"text\":\"\"}".into();
    };
    s.stop.store(true, Ordering::Relaxed);
    // Best effort: wait for the engine to finalize the last phrase (grace
    // period inside the worker), but never hang the control channel.
    if let Some(done) = s.done.take() {
        let _ = done.recv_timeout(Duration::from_secs(6));
    }
    let text = s.results.lock().expect("speech results").clone();
    serde_json::json!({"ok": true, "text": text}).to_string()
}

pub fn status() -> String {
    let active = SPEECH.lock().expect("speech session").is_some();
    serde_json::json!({"ok": true, "active": active}).to_string()
}
