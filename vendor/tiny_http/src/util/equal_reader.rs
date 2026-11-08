use std::io::Read;
use std::io::Result as IoResult;
use std::sync::mpsc::channel;
use std::sync::mpsc::{Receiver, Sender};

/// A `Reader` that reads exactly the number of bytes from a sub-reader.
///
/// If the limit is reached, it returns EOF. If the limit is not reached
/// when the destructor is called, the remaining bytes will be read and
/// thrown away.
pub struct EqualReader<R>
where
    R: Read,
{
    reader: R,
    size: usize,
    last_read_signal: Sender<IoResult<()>>,
}

impl<R> EqualReader<R>
where
    R: Read,
{
    pub fn new(reader: R, size: usize) -> (EqualReader<R>, Receiver<IoResult<()>>) {
        let (tx, rx) = channel();

        let r = EqualReader {
            reader,
            size,
            last_read_signal: tx,
        };

        (r, rx)
    }
}

impl<R> Read for EqualReader<R>
where
    R: Read,
{
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        if self.size == 0 {
            return Ok(0);
        }

        let buf = if buf.len() < self.size {
            buf
        } else {
            &mut buf[..self.size]
        };

        match self.reader.read(buf) {
            Ok(len) => {
                self.size -= len;
                Ok(len)
            }
            err @ Err(_) => err,
        }
    }
}

impl<R> Drop for EqualReader<R>
where
    R: Read,
{
    // [wind-setting 补丁] 原实现按剩余声明长度一次性 `vec![0; remaining]`：离谱的
    // Content-Length 会让分配失败、整个进程 abort。改为固定 8 KiB 缓冲，且最多再丢弃
    // DRAIN_BUDGET 字节；超出预算就不再读，连接上残留的字节会让下一次请求解析失败、
    // 连接随之关闭。见仓库根 vendor/tiny_http/PATCHES.md。
    fn drop(&mut self) {
        const DRAIN_BUDGET: usize = 1024 * 1024;
        let mut buf = [0u8; 8 * 1024];
        let mut budget = DRAIN_BUDGET;

        while self.size > 0 {
            if budget == 0 {
                let err = std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "request body too large to discard",
                );
                self.last_read_signal.send(Err(err)).ok();
                return;
            }
            let want = self.size.min(buf.len()).min(budget);
            match self.reader.read(&mut buf[..want]) {
                Err(e) => {
                    self.last_read_signal.send(Err(e)).ok();
                    return;
                }
                Ok(0) => {
                    self.last_read_signal.send(Ok(())).ok();
                    return;
                }
                Ok(n) => {
                    self.size -= n;
                    budget -= n;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EqualReader;
    use std::io::Read;

    #[test]
    fn test_limit() {
        use std::io::Cursor;

        let mut org_reader = Cursor::new("hello world".to_string().into_bytes());

        {
            let (mut equal_reader, _) = EqualReader::new(org_reader.by_ref(), 5);

            let mut string = String::new();
            equal_reader.read_to_string(&mut string).unwrap();
            assert_eq!(string, "hello");
        }

        let mut string = String::new();
        org_reader.read_to_string(&mut string).unwrap();
        assert_eq!(string, " world");
    }

    #[test]
    fn test_not_enough() {
        use std::io::Cursor;

        let mut org_reader = Cursor::new("hello world".to_string().into_bytes());

        {
            let (mut equal_reader, _) = EqualReader::new(org_reader.by_ref(), 5);

            let mut vec = [0];
            equal_reader.read_exact(&mut vec).unwrap();
            assert_eq!(vec[0], b'h');
        }

        let mut string = String::new();
        org_reader.read_to_string(&mut string).unwrap();
        assert_eq!(string, " world");
    }
}
