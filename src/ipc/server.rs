use crate::error::Result;
use serde::{de::DeserializeOwned, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

pub fn read_line_json<T: DeserializeOwned>(stream: &UnixStream) -> Result<T> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let msg = serde_json::from_str::<T>(line.trim_end())?;
    Ok(msg)
}

pub fn write_line_json<T: Serialize>(stream: &mut UnixStream, msg: &T) -> Result<()> {
    let payload = serde_json::to_string(msg)?;
    stream.write_all(payload.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}
