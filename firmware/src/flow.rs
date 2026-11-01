//! How many packets are still in the Bluetooth controller.
//!
//! It accepts packets faster than a slow link sends them, and whatever waits
//! in its buffers can't be merged or dropped any more. trouble tracks its
//! free buffers but doesn't expose them, so [`Observed`] sits between it and
//! the radio driver and counts packets going in against the controller's
//! reports of packets sent. The BLE task uses that to hold moves back while
//! the controller is busy, which adapts to whatever the link can do.

use bt_hci::transport::{PacketToController, PacketToHost, Transport};
use bt_hci::{PacketKind, ReadHciError};
use deskflow_proto::flow::completed_packets;
use embedded_io::{ErrorType, Read};
use embedded_io_async::Read as AsyncRead;
use portable_atomic::{AtomicU32, Ordering};

use crate::MOTION_READY;

/// Packets the controller may hold before moves wait. One on its way and one
/// behind it: its reports lag the radio, so waiting for empty wastes events.
const ROOM: u32 = 2;

/// Enough for a report covering several connections.
const EVENT_KEPT: usize = 32;

static IN_CONTROLLER: AtomicU32 = AtomicU32::new(0);

/// Packets not yet reported sent.
pub fn in_controller() -> u32 {
    IN_CONTROLLER.load(Ordering::Relaxed)
}

/// Whether the controller has room for a move.
pub fn has_room() -> bool {
    in_controller() < ROOM
}

/// For a new connection: what the last one left behind is never reported.
pub fn reset() {
    IN_CONTROLLER.store(0, Ordering::Relaxed);
}

fn note_sent() {
    IN_CONTROLLER.add(1, Ordering::Relaxed);
}

fn note_completed(packets: u32) {
    let left = IN_CONTROLLER.load(Ordering::Relaxed).saturating_sub(packets);
    IN_CONTROLLER.store(left, Ordering::Relaxed);
    MOTION_READY.signal(());
}

/// A transport that keeps count of the packets in the controller.
pub struct Observed<T>(pub T);

impl<T: ErrorType> ErrorType for Observed<T> {
    type Error = T::Error;
}

impl<T: Transport> Transport for Observed<T> {
    async fn read<'a, P: PacketToHost<'a>>(&self, rx: &'a mut [u8]) -> Result<P, Self::Error> {
        self.0.read::<Spied<P>>(rx).await.map(|Spied(packet)| packet)
    }

    async fn write<P: PacketToController>(&self, tx: &P) -> Result<(), Self::Error> {
        let result = self.0.write(tx).await;
        if P::KIND == PacketKind::AclData && result.is_ok() {
            note_sent();
        }
        result
    }
}

/// Parses a packet as `P` would, noticing "packets sent" reports on the way.
struct Spied<P>(P);

impl<'a, P: PacketToHost<'a>> PacketToHost<'a> for Spied<P> {
    fn read_hci<R: Read>(kind: PacketKind, data: &mut R, buf: &'a mut [u8]) -> Result<Self, ReadHciError<R::Error>> {
        let mut tee = Tee { inner: data, kept: [0; EVENT_KEPT], len: 0 };
        let packet = P::read_hci(kind, &mut tee, buf)?;
        if kind == PacketKind::Event
            && let Some(packets) = completed_packets(&tee.kept[..tee.len])
        {
            note_completed(packets);
        }
        Ok(Spied(packet))
    }

    /// Not spied on: the cyw43 driver only uses [`PacketToHost::read_hci`].
    async fn read_hci_async<R: AsyncRead>(
        kind: PacketKind,
        data: &mut R,
        buf: &'a mut [u8],
    ) -> Result<Self, ReadHciError<R::Error>> {
        P::read_hci_async(kind, data, buf).await.map(Spied)
    }
}

/// A reader that keeps the first bytes read through it.
struct Tee<'r, R> {
    inner: &'r mut R,
    kept: [u8; EVENT_KEPT],
    len: usize,
}

impl<R: ErrorType> ErrorType for Tee<'_, R> {
    type Error = R::Error;
}

impl<R: Read> Read for Tee<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let n = self.inner.read(buf)?;
        let room = (EVENT_KEPT - self.len).min(n);
        self.kept[self.len..self.len + room].copy_from_slice(&buf[..room]);
        self.len += room;
        Ok(n)
    }
}
