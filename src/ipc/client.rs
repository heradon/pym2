use crate::error::{PyopsError, Result};
use crate::ipc::server::{read_line_json, write_line_json};
use crate::model::{AgentEvent, IpcRequest, IpcResponse, StreamLogEvent};
use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct IpcClient {
    socket_path: PathBuf,
}

impl IpcClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    pub fn request(&self, req: IpcRequest) -> Result<IpcResponse> {
        let mut stream = UnixStream::connect(&self.socket_path).map_err(|e| {
            PyopsError::Ipc(format!(
                "failed to connect to agent socket {}: {}",
                self.socket_path.display(),
                e
            ))
        })?;
        write_line_json(&mut stream, &req)?;
        let resp: IpcResponse = read_line_json(&stream)?;
        Ok(resp)
    }

    pub fn stream_logs<F: FnMut(StreamLogEvent)>(
        &self,
        req: IpcRequest,
        mut on_line: F,
    ) -> Result<()> {
        self.stream(req, move |data| {
            let event: StreamLogEvent = serde_json::from_value(data)?;
            on_line(event);
            Ok(())
        })
    }

    pub fn stream_events<F: FnMut(AgentEvent)>(&self, mut on_event: F) -> Result<()> {
        self.stream(IpcRequest::WatchEvents, move |data| {
            let event: AgentEvent = serde_json::from_value(data)?;
            on_event(event);
            Ok(())
        })
    }

    pub fn stream_logs_until<F, G>(
        &self,
        req: IpcRequest,
        mut should_stop: G,
        mut on_line: F,
    ) -> Result<()>
    where
        F: FnMut(StreamLogEvent),
        G: FnMut() -> bool,
    {
        self.stream_until(
            req,
            move || should_stop(),
            move |data| {
                let event: StreamLogEvent = serde_json::from_value(data)?;
                on_line(event);
                Ok(())
            },
        )
    }

    fn stream<F>(&self, req: IpcRequest, mut on_item: F) -> Result<()>
    where
        F: FnMut(serde_json::Value) -> Result<()>,
    {
        self.stream_until(req, || false, move |data| on_item(data))
    }

    fn stream_until<F, G>(&self, req: IpcRequest, mut should_stop: G, mut on_item: F) -> Result<()>
    where
        F: FnMut(serde_json::Value) -> Result<()>,
        G: FnMut() -> bool,
    {
        let mut stream = UnixStream::connect(&self.socket_path).map_err(|e| {
            PyopsError::Ipc(format!(
                "failed to connect to agent socket {}: {}",
                self.socket_path.display(),
                e
            ))
        })?;

        stream.set_read_timeout(Some(Duration::from_millis(250)))?;
        write_line_json(&mut stream, &req)?;

        let mut reader = std::io::BufReader::new(stream);
        let mut line = String::new();
        loop {
            if should_stop() {
                break;
            }

            line.clear();
            let read = match std::io::BufRead::read_line(&mut reader, &mut line) {
                Ok(read) => read,
                Err(err)
                    if matches!(
                        err.kind(),
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(err) => return Err(PyopsError::Io(err)),
            };
            if read == 0 {
                break;
            }

            let resp: IpcResponse = serde_json::from_str(line.trim_end())?;
            if !resp.ok {
                return Err(PyopsError::Ipc(resp.error.unwrap_or_else(|| {
                    "log stream returned unknown error".to_string()
                })));
            }

            if let Some(data) = resp.data {
                on_item(data)?;
            }
        }

        Ok(())
    }
}
