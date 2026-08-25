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
use std::time::Duration;

use windows::Foundation::TypedEventHandler;
use windows::Media::SpeechRecognition::{
    SpeechContinuousRecognitionResultGeneratedEventArgs, SpeechContinuousRecognitionSession,
    SpeechRecognitionResultStatus, SpeechRecognizer,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

struct SpeechSession {
    stop: Arc<AtomicBool>,
    results: Arc<Mutex<String>>,
    join: Option<thread::JoinHandle<()>>,
}

static SPEECH: Mutex<Option<SpeechSession>> = Mutex::new(None);

/// Runs on the worker thread after COM is initialized. Sends the outcome of
/// the start attempt on `started` (Ok once recognition is live), then parks
/// until the stop flag, then stops the session cleanly on the same thread
/// that created it.
fn run_session(
    stop: &AtomicBool,
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
    let handler = TypedEventHandler::<
        SpeechContinuousRecognitionSession,
        SpeechContinuousRecognitionResultGeneratedEventArgs,
    >::new(move |_, args| {
        // Keep the latest partial/final text; /speech/stop returns whatever
        // the engine most recently heard.
        if let Some(args) = args.as_ref() {
            if let Ok(result) = args.Result() {
                if let Ok(text) = result.Text() {
                    *results_for_event.lock().expect("speech results") = text.to_string();
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
    let _ = session.StopAsync().and_then(|op| op.get());
    let _ = session.RemoveResultGenerated(token);
}

fn speech_thread(
    stop: Arc<AtomicBool>,
    results: Arc<Mutex<String>>,
    started: mpsc::Sender<Result<(), String>>,
) {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if com.is_ok() {
        run_session(&stop, results, &started);
    } else {
        let _ = started.send(Err(format!("COM 初始化失败：{com}")));
    }
    unsafe { CoUninitialize() };
}

/// Begin listening. Idempotent from the caller's view: any prior session is
/// stopped first, so a new hold-to-talk press always starts clean.
pub fn start() -> String {
    let mut guard = SPEECH.lock().expect("speech session");
    if let Some(old) = guard.take() {
        old.stop.store(true, Ordering::Relaxed);
        if let Some(join) = old.join {
            let _ = join.join();
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let results = Arc::new(Mutex::new(String::new()));
    let (tx, rx) = mpsc::channel();
    let join = thread::spawn({
        let stop = stop.clone();
        let results = results.clone();
        move || speech_thread(stop, results, tx)
    });
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {
            *guard = Some(SpeechSession { stop, results, join: Some(join) });
            "{\"ok\":true}".into()
        }
        Ok(Err(msg)) => {
            let _ = join.join();
            serde_json::json!({"ok": false, "error": msg}).to_string()
        }
        Err(_) => {
            // The worker is wedged (start never completed). Flag it so it
            // exits if it ever unblocks, and keep the handle so a later
            // /speech/stop can still join it.
            stop.store(true, Ordering::Relaxed);
            *guard = Some(SpeechSession { stop, results, join: Some(join) });
            "{\"ok\":false,\"error\":\"语音识别启动超时\"}".into()
        }
    }
}

/// Stop listening and return the accumulated text (may be empty if nothing
/// was recognized, e.g. the press was too short or speech never started).
pub fn stop() -> String {
    let mut guard = SPEECH.lock().expect("speech session");
    let Some(mut s) = guard.take() else {
        return "{\"ok\":true,\"text\":\"\"}".into();
    };
    s.stop.store(true, Ordering::Relaxed);
    if let Some(join) = s.join.take() {
        let _ = join.join();
    }
    let text = s.results.lock().expect("speech results").clone();
    serde_json::json!({"ok": true, "text": text}).to_string()
}

pub fn status() -> String {
    let active = SPEECH.lock().expect("speech session").is_some();
    serde_json::json!({"ok": true, "active": active}).to_string()
}
