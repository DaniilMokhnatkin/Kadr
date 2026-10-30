//! Persistent, hidden offline EN/RU worker. Models and runtime live next to the installed app.
use super::translate::Translation;
use std::os::windows::process::CommandExt;
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Mutex, OnceLock, mpsc},
    time::Duration,
};

fn folder() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let installed = exe.parent()?.join("translation");
    if installed.join("python/python.exe").is_file() {
        return Some(installed);
    }
    if cfg!(debug_assertions) || exe.parent()?.ends_with("release") {
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/translation");
        if dev.join("python/python.exe").is_file() {
            return Some(dev);
        }
    }
    None
}
pub fn available() -> bool {
    folder().is_some_and(|dir| {
        dir.join("translation-worker.py").is_file() && dir.join("models").is_dir()
    })
}
struct Worker {
    child: Child,
    input: ChildStdin,
    replies: mpsc::Receiver<Result<serde_json::Value, String>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
static WORKER: OnceLock<Mutex<Option<Worker>>> = OnceLock::new();
fn start() -> Result<Worker, String> {
    let dir = folder()
        .ok_or("Офлайн-перевод не установлен. Добавьте языковые модели через установщик Kadr.")?;
    let mut child = Command::new(dir.join("python/python.exe"))
        .args(["-I", "-u"])
        .arg(dir.join("translation-worker.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(0x08004000)
        .spawn()
        .map_err(|e| format!("Не удалось запустить офлайн-перевод: {e}"))?;
    let input = child.stdin.take().ok_or("Нет канала к переводчику")?;
    let output = child.stdout.take().ok_or("Нет ответа переводчика")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let value = line
                .map_err(|e| e.to_string())
                .and_then(|line| serde_json::from_str(&line).map_err(|e| e.to_string()));
            if tx.send(value).is_err() {
                break;
            }
        }
    });
    Ok(Worker {
        child,
        input,
        replies: rx,
    })
}
pub fn translate(text: &str, to: &str) -> Result<Translation, String> {
    let mut slot = WORKER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "Офлайн-перевод занят")?;
    if slot.is_none() {
        *slot = Some(start()?);
    }
    let result = (|| {
        let worker = slot.as_mut().unwrap();
        serde_json::to_writer(&mut worker.input, &serde_json::json!({"text":text,"to":to}))
            .map_err(|e| e.to_string())?;
        worker
            .input
            .write_all(b"\n")
            .and_then(|()| worker.input.flush())
            .map_err(|e| e.to_string())?;
        let reply = worker
            .replies
            .recv_timeout(Duration::from_secs(90))
            .map_err(|_| {
                "Офлайн-перевод не ответил вовремя. Попробуйте выделить меньше текста.".to_string()
            })??;
        if let Some(error) = reply.get("error").and_then(|v| v.as_str()) {
            return Err(error.to_string());
        }
        Ok(Translation {
            text: reply
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or("Некорректный ответ офлайн-переводчика")?
                .to_string(),
            from: reply
                .get("from")
                .and_then(|v| v.as_str())
                .unwrap_or("en")
                .to_string(),
            to: to.into(),
        })
    })();
    if result.is_err() {
        *slot = None;
    }
    result
}
pub fn shutdown() {
    if let Some(worker) = WORKER.get()
        && let Ok(mut worker) = worker.try_lock()
    {
        *worker = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires the optional local EN/RU runtime and models"]
    fn actual_local_models_translate_in_both_directions_without_http() {
        assert!(available());
        let ru = translate("The screenshot was saved successfully.", "ru").unwrap();
        assert!(
            ru.text.chars().any(|c| ('а'..='я').contains(&c)),
            "{}",
            ru.text
        );
        let en = translate("Снимок экрана успешно сохранён.", "en").unwrap();
        assert!(en.text.is_ascii(), "{}", en.text);
        eprintln!("EN -> RU: {} / RU -> EN: {}", ru.text, en.text);
        shutdown();
    }
}
