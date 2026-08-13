use std::collections::{BTreeSet, HashMap, VecDeque};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use wasmtime::{Engine, InstancePre, Store, StoreLimitsBuilder};
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::cli::{IsTerminal, StdinStream, StdoutStream};
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;

use crate::TypeScriptConfig;
use crate::runtime::State;

const ROOT: &str = "/app";
const PIPE_BUFFER: usize = 1 << 20;

const MSG_REQUEST: u8 = 1;
const MSG_CALL_RESPONSE: u8 = 2;
const MSG_RESPONSE: u8 = 4;
const MSG_ERROR: u8 = 5;
const MSG_CALL: u8 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Category {
    Warning,
    #[default]
    Error,
    Suggestion,
    Message,
}

impl Category {
    pub fn is_error(self) -> bool {
        matches!(self, Category::Error)
    }
}

impl<'de> serde::Deserialize<'de> for Category {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(match i64::deserialize(deserializer)? {
            0 => Category::Warning,
            1 => Category::Error,
            2 => Category::Suggestion,
            _ => Category::Message,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    #[serde(default)]
    pub file_name: Option<String>,
    pub pos: i64,
    pub end: i64,
    #[serde(skip)]
    pub range: Option<Range>,
    pub code: i32,
    #[serde(default)]
    pub category: Category,
    pub text: String,
    #[serde(default)]
    pub message_chain: Vec<Diagnostic>,
    #[serde(default)]
    pub related_information: Vec<Diagnostic>,
}

impl Diagnostic {
    pub fn is_error(&self) -> bool {
        self.category.is_error()
    }
}

fn position_at(source: &str, utf16_offset: i64) -> Position {
    let target = utf16_offset.max(0) as usize;
    let mut units = 0usize;
    let mut line = 1u32;
    let mut column = 1u32;
    for ch in source.chars() {
        if units >= target {
            break;
        }
        units += ch.len_utf16();
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += ch.len_utf16() as u32;
        }
    }
    Position { line, column }
}

fn enrich(diagnostic: &mut Diagnostic, files: &HashMap<String, String>) {
    if let Some(full) = diagnostic.file_name.clone() {
        if let Some(source) = files.get(&full) {
            diagnostic.range = Some(Range {
                start: position_at(source, diagnostic.pos),
                end: position_at(source, diagnostic.end),
            });
        }
        if let Some(stripped) = full.strip_prefix(&format!("{ROOT}/")) {
            diagnostic.file_name = Some(stripped.to_string());
        }
    }
    for child in &mut diagnostic.message_chain {
        enrich(child, files);
    }
    for child in &mut diagnostic.related_information {
        enrich(child, files);
    }
}

type Files = Arc<Mutex<HashMap<String, String>>>;

#[derive(Default)]
struct StdinState {
    buffer: VecDeque<u8>,
    closed: bool,
    wakers: Vec<Waker>,
}

#[derive(Clone, Default)]
struct HostStdin(Arc<Mutex<StdinState>>);

impl HostStdin {
    fn push(&self, bytes: &[u8]) -> anyhow::Result<()> {
        let mut state = self.0.lock().unwrap();
        if state.closed {
            bail!("tsgo api: session closed");
        }
        state.buffer.extend(bytes);
        for waker in state.wakers.drain(..) {
            waker.wake();
        }
        Ok(())
    }

    fn close(&self) {
        let mut state = self.0.lock().unwrap();
        state.closed = true;
        for waker in state.wakers.drain(..) {
            waker.wake();
        }
    }
}

#[async_trait::async_trait]
impl wasmtime_wasi::p2::InputStream for HostStdin {
    fn read(&mut self, size: usize) -> wasmtime_wasi::p2::StreamResult<bytes::Bytes> {
        let mut state = self.0.lock().unwrap();
        if state.buffer.is_empty() {
            if state.closed {
                return Err(wasmtime_wasi::p2::StreamError::Closed);
            }
            return Ok(bytes::Bytes::new());
        }
        let take = size.min(state.buffer.len());
        let out: Vec<u8> = state.buffer.drain(..take).collect();
        Ok(out.into())
    }
}

#[async_trait::async_trait]
impl wasmtime_wasi::p2::Pollable for HostStdin {
    async fn ready(&mut self) {
        std::future::poll_fn(|cx| {
            let mut state = self.0.lock().unwrap();
            if !state.buffer.is_empty() || state.closed {
                Poll::Ready(())
            } else {
                state.wakers.push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}

impl AsyncRead for HostStdin {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let mut state = self.0.lock().unwrap();
        if !state.buffer.is_empty() {
            let take = state.buffer.len().min(buf.remaining());
            for byte in state.buffer.drain(..take) {
                buf.put_slice(&[byte]);
            }
            return Poll::Ready(Ok(()));
        }
        if state.closed {
            return Poll::Ready(Ok(()));
        }
        state.wakers.push(cx.waker().clone());
        Poll::Pending
    }
}

impl IsTerminal for HostStdin {
    fn is_terminal(&self) -> bool {
        false
    }
}

impl StdinStream for HostStdin {
    fn p2_stream(&self) -> Box<dyn wasmtime_wasi::p2::InputStream> {
        Box::new(self.clone())
    }

    fn async_stream(&self) -> Box<dyn AsyncRead + Send + Sync> {
        Box::new(self.clone())
    }
}

#[derive(Default)]
struct StdoutState {
    buffer: Vec<u8>,
    closed: bool,
}

#[derive(Clone, Default)]
struct HostStdout(Arc<(Mutex<StdoutState>, Condvar)>);

impl HostStdout {
    fn drain_deadline(&self, deadline: Instant) -> anyhow::Result<Option<Vec<u8>>> {
        let (mutex, condvar) = &*self.0;
        let mut state = mutex.lock().unwrap();
        loop {
            if !state.buffer.is_empty() {
                return Ok(Some(std::mem::take(&mut state.buffer)));
            }
            if state.closed {
                return Ok(None);
            }
            let now = Instant::now();
            if now >= deadline {
                bail!("timed out");
            }
            state = condvar.wait_timeout(state, deadline - now).unwrap().0;
        }
    }

    fn close(&self) {
        let (mutex, condvar) = &*self.0;
        mutex.lock().unwrap().closed = true;
        condvar.notify_all();
    }
}

impl AsyncWrite for HostStdout {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let (mutex, condvar) = &*self.0;
        mutex.lock().unwrap().buffer.extend_from_slice(bytes);
        condvar.notify_all();
        Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.close();
        Poll::Ready(Ok(()))
    }
}

#[async_trait::async_trait]
impl wasmtime_wasi::p2::OutputStream for HostStdout {
    fn write(&mut self, bytes: bytes::Bytes) -> wasmtime_wasi::p2::StreamResult<()> {
        let (mutex, condvar) = &*self.0;
        mutex.lock().unwrap().buffer.extend_from_slice(&bytes);
        condvar.notify_all();
        Ok(())
    }

    fn flush(&mut self) -> wasmtime_wasi::p2::StreamResult<()> {
        Ok(())
    }

    fn check_write(&mut self) -> wasmtime_wasi::p2::StreamResult<usize> {
        Ok(usize::MAX)
    }
}

#[async_trait::async_trait]
impl wasmtime_wasi::p2::Pollable for HostStdout {
    async fn ready(&mut self) {}
}

impl IsTerminal for HostStdout {
    fn is_terminal(&self) -> bool {
        false
    }
}

impl StdoutStream for HostStdout {
    fn p2_stream(&self) -> Box<dyn wasmtime_wasi::p2::OutputStream> {
        Box::new(self.clone())
    }

    fn async_stream(&self) -> Box<dyn AsyncWrite + Send + Sync> {
        Box::new(self.clone())
    }
}

pub struct ApiSession {
    files: Files,
    stdin: HostStdin,
    stdout: HostStdout,
    rx: Vec<u8>,
    timeout: Duration,
    snapshot: Value,
    project: Value,
    stderr: MemoryOutputPipe,
    guest_exit: Arc<Mutex<Option<String>>>,
    kill: Arc<AtomicBool>,
}

impl ApiSession {
    pub(crate) fn start(
        engine: Engine,
        instance_pre: InstancePre<State>,
        config: TypeScriptConfig,
        sources: &[(&str, &str)],
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        let mut map = HashMap::new();
        for (name, content) in sources {
            anyhow::ensure!(
                !name.starts_with('/') && !name.split('/').any(|part| part == ".."),
                "invalid source path: {name}"
            );
            map.insert(format!("{ROOT}/{name}"), content.to_string());
        }
        map.entry(format!("{ROOT}/tsconfig.json"))
            .or_insert_with(|| "{}".to_string());
        let files: Files = Arc::new(Mutex::new(map));

        let stdin = HostStdin::default();
        let stdout = HostStdout::default();
        let stderr = MemoryOutputPipe::new(PIPE_BUFFER);

        let mut builder = WasiCtxBuilder::new();
        builder.args(&[
            "tsgo",
            "--api",
            "--callbacks",
            "readFile,fileExists,directoryExists,getAccessibleEntries,realpath",
            "--cwd",
            ROOT,
        ]);
        builder.stdin(stdin.clone());
        builder.stdout(stdout.clone());
        builder.stderr(stderr.clone());

        let mut limits = StoreLimitsBuilder::new();
        if let Some(memory) = config.memory_limit {
            limits = limits.memory_size(memory);
        }
        let state = State {
            wasi: builder.build_p1(),
            limits: limits.build(),
        };

        let guest_exit: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let kill = Arc::new(AtomicBool::new(false));
        {
            let guest_exit = guest_exit.clone();
            let stdout_on_exit = stdout.clone();
            let kill = kill.clone();
            std::thread::spawn(move || {
                let mut store = Store::new(&engine, state);
                store.limiter(|state| &mut state.limits);
                store.set_epoch_deadline(1);
                store.epoch_deadline_callback(move |_| {
                    if kill.load(Ordering::Relaxed) {
                        return Err(wasmtime::Error::msg("tsgo api: session killed"));
                    }
                    Ok(wasmtime::UpdateDeadline::Continue(1))
                });
                let mut run = || -> anyhow::Result<()> {
                    let instance = instance_pre.instantiate(&mut store)?;
                    let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
                    start.call(&mut store, ())?;
                    Ok(())
                };
                let outcome = match run() {
                    Ok(()) => "exited cleanly".to_string(),
                    Err(error) => format!("{error:?}"),
                };
                *guest_exit.lock().unwrap() = Some(outcome);
                stdout_on_exit.close();
            });
        }

        let mut session = Self {
            files,
            stdin,
            stdout,
            rx: Vec::new(),
            timeout,
            snapshot: Value::Null,
            project: Value::Null,
            stderr,
            guest_exit,
            kill,
        };

        session.request("initialize", Value::Null)?;
        let opened = session.request(
            "updateSnapshot",
            json!({ "openProjects": [format!("{ROOT}/tsconfig.json")] }),
        )?;
        session.adopt_snapshot(opened)?;
        Ok(session)
    }

    fn adopt_snapshot(&mut self, response: Value) -> anyhow::Result<()> {
        let project = response
            .pointer("/projects/0/id")
            .cloned()
            .ok_or_else(|| anyhow!("tsgo api: no project in snapshot: {response}"))?;
        self.snapshot = response
            .get("snapshot")
            .cloned()
            .ok_or_else(|| anyhow!("tsgo api: no snapshot id: {response}"))?;
        self.project = project;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let payload = if params.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&params)?
        };
        self.write_tuple(MSG_REQUEST, method, &payload)?;

        let deadline = Instant::now() + self.timeout;
        loop {
            let (kind, tuple_method, payload) = self.read_tuple(deadline, method)?;
            match kind {
                MSG_CALL => {
                    let answer = answer_callback(&self.files, &tuple_method, &payload);
                    self.write_tuple(
                        MSG_CALL_RESPONSE,
                        &tuple_method,
                        &serde_json::to_vec(&answer)?,
                    )?;
                }
                MSG_RESPONSE => {
                    anyhow::ensure!(
                        tuple_method == method,
                        "tsgo api: response for {tuple_method}, expected {method}"
                    );
                    if payload.is_empty() {
                        return Ok(Value::Null);
                    }
                    return Ok(serde_json::from_slice(&payload).unwrap_or(Value::Null));
                }
                MSG_ERROR => {
                    bail!(
                        "tsgo api: {method} failed: {}",
                        String::from_utf8_lossy(&payload)
                    );
                }
                other => bail!("tsgo api: unexpected message type {other}"),
            }
        }
    }

    fn write_tuple(&self, kind: u8, method: &str, payload: &[u8]) -> anyhow::Result<()> {
        let mut frame = Vec::with_capacity(2 + 5 + method.len() + 5 + payload.len());
        frame.push(0x93);
        frame.push(kind);
        write_bin(&mut frame, method.as_bytes());
        write_bin(&mut frame, payload);
        self.stdin.push(&frame)
    }

    fn read_tuple(
        &mut self,
        deadline: Instant,
        method: &str,
    ) -> anyhow::Result<(u8, String, Vec<u8>)> {
        loop {
            if let Some((kind, tuple_method, payload, consumed)) = parse_tuple(&self.rx)? {
                self.rx.drain(..consumed);
                return Ok((kind, tuple_method, payload));
            }
            match self.stdout.drain_deadline(deadline) {
                Ok(Some(chunk)) => self.rx.extend_from_slice(&chunk),
                Ok(None) => bail!(
                    "tsgo api: {method}: server exited: {:?}; stderr: {}",
                    self.guest_exit.lock().unwrap(),
                    String::from_utf8_lossy(&self.stderr.contents())
                ),
                Err(_) => {
                    self.kill.store(true, Ordering::Relaxed);
                    bail!(
                        "tsgo api: {method} timed out after {:?}; session killed; guest: {:?}; stderr: {}",
                        self.timeout,
                        self.guest_exit.lock().unwrap(),
                        String::from_utf8_lossy(&self.stderr.contents())
                    )
                }
            }
        }
    }

    pub fn diagnostics(&mut self) -> anyhow::Result<Vec<Diagnostic>> {
        let params = json!({ "snapshot": self.snapshot, "project": self.project });
        let mut all = Vec::new();
        for method in [
            "getConfigFileParsingDiagnostics",
            "getSyntacticDiagnostics",
            "getSemanticDiagnostics",
            "getGlobalDiagnostics",
            "getProgramDiagnostics",
        ] {
            let result = self.request(method, params.clone())?;
            if result.is_null() {
                continue;
            }
            let mut batch: Vec<Diagnostic> = serde_json::from_value(result)?;
            all.append(&mut batch);
        }
        let files = self.files.lock().unwrap();
        for diagnostic in &mut all {
            enrich(diagnostic, &files);
        }
        Ok(all)
    }

    pub fn diagnostics_for(&mut self, name: &str) -> anyhow::Result<Vec<Diagnostic>> {
        let params = json!({
            "snapshot": self.snapshot,
            "project": self.project,
            "file": format!("{ROOT}/{name}"),
        });
        let mut all = Vec::new();
        for method in ["getSyntacticDiagnostics", "getSemanticDiagnostics"] {
            let result = self.request(method, params.clone())?;
            if result.is_null() {
                continue;
            }
            let mut batch: Vec<Diagnostic> = serde_json::from_value(result)?;
            all.append(&mut batch);
        }
        let files = self.files.lock().unwrap();
        for diagnostic in &mut all {
            enrich(diagnostic, &files);
        }
        Ok(all)
    }

    pub fn update_file(&mut self, name: &str, content: &str) -> anyhow::Result<()> {
        let path = format!("{ROOT}/{name}");
        let existed = self
            .files
            .lock()
            .unwrap()
            .insert(path.clone(), content.to_string())
            .is_some();
        let kind = if existed { "changed" } else { "created" };
        let response =
            self.request("updateSnapshot", json!({ "fileChanges": { kind: [path] } }))?;
        self.adopt_snapshot(response)
    }

    pub fn remove_file(&mut self, name: &str) -> anyhow::Result<()> {
        let path = format!("{ROOT}/{name}");
        self.files.lock().unwrap().remove(&path);
        let response = self.request(
            "updateSnapshot",
            json!({ "fileChanges": { "deleted": [path] } }),
        )?;
        self.adopt_snapshot(response)
    }
}

impl Drop for ApiSession {
    fn drop(&mut self) {
        self.stdin.close();
        self.kill.store(true, Ordering::Relaxed);
    }
}

fn write_bin(out: &mut Vec<u8>, bytes: &[u8]) {
    match bytes.len() {
        len if len <= u8::MAX as usize => {
            out.push(0xC4);
            out.push(len as u8);
        }
        len if len <= u16::MAX as usize => {
            out.push(0xC5);
            out.extend_from_slice(&(len as u16).to_be_bytes());
        }
        len => {
            out.push(0xC6);
            out.extend_from_slice(&(len as u32).to_be_bytes());
        }
    }
    out.extend_from_slice(bytes);
}

fn parse_tuple(buffer: &[u8]) -> anyhow::Result<Option<(u8, String, Vec<u8>, usize)>> {
    let mut pos = 0usize;
    let Some(&marker) = buffer.get(pos) else {
        return Ok(None);
    };
    anyhow::ensure!(marker == 0x93, "tsgo api: bad tuple marker 0x{marker:02x}");
    pos += 1;

    let Some(&type_byte) = buffer.get(pos) else {
        return Ok(None);
    };
    pos += 1;
    let kind = if type_byte <= 0x7F {
        type_byte
    } else if type_byte == 0xCC {
        let Some(&value) = buffer.get(pos) else {
            return Ok(None);
        };
        pos += 1;
        value
    } else {
        bail!("tsgo api: bad message type marker 0x{type_byte:02x}");
    };

    let Some((method_bytes, next)) = parse_bin(buffer, pos)? else {
        return Ok(None);
    };
    pos = next;
    let Some((payload, next)) = parse_bin(buffer, pos)? else {
        return Ok(None);
    };
    pos = next;

    Ok(Some((
        kind,
        String::from_utf8_lossy(&method_bytes).into_owned(),
        payload,
        pos,
    )))
}

fn parse_bin(buffer: &[u8], mut pos: usize) -> anyhow::Result<Option<(Vec<u8>, usize)>> {
    let Some(&marker) = buffer.get(pos) else {
        return Ok(None);
    };
    pos += 1;
    let (length, header) = match marker {
        0xC4 => {
            let Some(&len) = buffer.get(pos) else {
                return Ok(None);
            };
            (len as usize, 1)
        }
        0xC5 => {
            let Some(len) = buffer.get(pos..pos + 2) else {
                return Ok(None);
            };
            (u16::from_be_bytes([len[0], len[1]]) as usize, 2)
        }
        0xC6 => {
            let Some(len) = buffer.get(pos..pos + 4) else {
                return Ok(None);
            };
            (
                u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize,
                4,
            )
        }
        other => bail!("tsgo api: bad bin marker 0x{other:02x}"),
    };
    pos += header;
    let Some(bytes) = buffer.get(pos..pos + length) else {
        return Ok(None);
    };
    Ok(Some((bytes.to_vec(), pos + length)))
}

fn in_scope(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    path == ROOT || path.starts_with(&format!("{ROOT}/"))
}

fn is_root_ancestor(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    path.is_empty() || ROOT.starts_with(&format!("{path}/"))
}

fn answer_callback(files: &Files, method: &str, payload: &[u8]) -> Value {
    let params: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
    let path = params.as_str().unwrap_or("");
    let files = files.lock().unwrap();
    match method {
        "readFile" if in_scope(path) => match files.get(path) {
            Some(content) => json!({ "content": content }),
            None => json!({ "content": null }),
        },
        "fileExists" if in_scope(path) => json!(files.contains_key(path)),
        "directoryExists" if in_scope(path) || is_root_ancestor(path) => {
            json!(directory_exists(&files, path))
        }
        "getAccessibleEntries" if in_scope(path) => accessible_entries(&files, path),
        "realpath" if in_scope(path) || is_root_ancestor(path) => json!(path),
        _ => Value::Null,
    }
}

fn directory_exists(files: &HashMap<String, String>, path: &str) -> bool {
    let path = path.trim_end_matches('/');
    if path.is_empty() || ROOT == path || ROOT.starts_with(&format!("{path}/")) {
        return true;
    }
    let prefix = format!("{path}/");
    files.keys().any(|key| key.starts_with(&prefix))
}

fn accessible_entries(files: &HashMap<String, String>, path: &str) -> Value {
    let prefix = format!("{}/", path.trim_end_matches('/'));
    let mut entries_files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    for key in files.keys() {
        if let Some(rest) = key.strip_prefix(&prefix) {
            match rest.split_once('/') {
                Some((dir, _)) => {
                    directories.insert(dir.to_string());
                }
                None => {
                    entries_files.insert(rest.to_string());
                }
            }
        }
    }
    json!({
        "files": entries_files.into_iter().collect::<Vec<_>>(),
        "directories": directories.into_iter().collect::<Vec<_>>(),
    })
}
