//! Ordered PTY input with a bounded budget, independent of the event loop.

use std::{
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

use anyhow::{Result, bail};

const MAX_PENDING_BYTES: usize = 32 * 1024 * 1024;
const MAX_PENDING_WRITES: usize = 256;

pub(super) struct PtyInput {
    sender: mpsc::SyncSender<Vec<u8>>,
    pending: Arc<AtomicUsize>,
}

impl PtyInput {
    pub(super) fn spawn(mut writer: Box<dyn Write + Send>) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(MAX_PENDING_WRITES);
        let pending = Arc::new(AtomicUsize::new(0));
        let queued = pending.clone();
        thread::spawn(move || {
            while let Ok(bytes) = receiver.recv() {
                let result = writer.write_all(&bytes).and_then(|()| writer.flush());
                queued.fetch_sub(bytes.len(), Ordering::Relaxed);
                if result.is_err() {
                    break;
                }
            }
        });
        Self { sender, pending }
    }

    /// Reject the entire input when full; never send a truncated paste.
    pub(super) fn send(&self, bytes: &[u8]) -> Result<()> {
        if self
            .pending
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
                pending
                    .checked_add(bytes.len())
                    .filter(|total| *total <= MAX_PENDING_BYTES)
            })
            .is_err()
        {
            bail!("pane input is full; input was not sent");
        }
        match self.sender.try_send(bytes.to_vec()) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.pending.fetch_sub(bytes.len(), Ordering::Relaxed);
                match error {
                    mpsc::TrySendError::Full(_) => bail!("pane input is full; input was not sent"),
                    mpsc::TrySendError::Disconnected(_) => bail!("pane input writer has stopped"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io, time::Duration};

    struct StalledWriter {
        entered: mpsc::SyncSender<()>,
        release: mpsc::Receiver<()>,
    }

    impl Write for StalledWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.entered.send(()).unwrap();
            self.release.recv().unwrap();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stalled_input_does_not_block_the_caller_and_has_a_byte_limit() {
        let (entered, waiting) = mpsc::sync_channel(1);
        let (release, resume) = mpsc::channel();
        let input = PtyInput::spawn(Box::new(StalledWriter {
            entered,
            release: resume,
        }));
        input.send(&vec![b'x'; MAX_PENDING_BYTES]).unwrap();
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            input
                .send(b"more")
                .unwrap_err()
                .to_string()
                .contains("full")
        );
        release.send(()).unwrap();
    }

    #[test]
    fn input_preserves_paste_boundaries_and_order() {
        let (writer, mut reader) = std::os::unix::net::UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let input = PtyInput::spawn(Box::new(writer));
        input.send(b"\x1b[200~one\x1b[201~").unwrap();
        input.send(b"two").unwrap();
        let mut received = [0; 18];
        std::io::Read::read_exact(&mut reader, &mut received).unwrap();
        assert_eq!(&received, b"\x1b[200~one\x1b[201~two");
    }
}
