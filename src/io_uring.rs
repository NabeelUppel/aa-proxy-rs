use crate::TCP_SERVER_PORT;
use bytesize::ByteSize;
use simplelog::*;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio::time::timeout;
use tokio_uring::buf::BoundedBuf;

// module name for logging engine
const NAME: &str = "<i><bright-black> proxy: </>";

const USB_ACCESSORY_PATH: &str = "/dev/usb_accessory";
const BUFFER_LEN: usize = 16 * 1024;
const READ_TIMEOUT: Duration = Duration::new(5, 0);

async fn copy_file_to_stream(
    from: Rc<tokio_uring::fs::File>,
    to: Rc<tokio_uring::net::TcpStream>,
    stats_interval: Option<Duration>,
) -> Result<(), std::io::Error> {
    // For statistics
    let mut bytes_out: usize = 0;
    let mut bytes_out_last: usize = 0;
    let mut report_time = Instant::now();

    let mut buf = vec![0u8; BUFFER_LEN];
    loop {
        // Handle stats printing
        if stats_interval.is_some() && report_time.elapsed() > stats_interval.unwrap() {
            let transferred_total = ByteSize::b(bytes_out.try_into().unwrap());
            let transferred_last = ByteSize::b(bytes_out_last.try_into().unwrap());

            let speed: u64 =
                (bytes_out_last as f64 / report_time.elapsed().as_secs_f64()).round() as u64;
            let speed = ByteSize::b(speed);

            info!(
                "{} 📲 car to phone transfer: {:#} ({:#}/s), {:#} total",
                NAME,
                transferred_last.to_string_as(true),
                speed.to_string_as(true),
                transferred_total.to_string_as(true),
            );

            report_time = Instant::now();
            bytes_out_last = 0;
        }

        // things look weird: we pass ownership of the buffer to `read`, and we get
        // it back, _even if there was an error_. There's a whole trait for that,
        // which `Vec<u8>` implements!
        debug!("USB: before read");
        let retval = from.read_at(buf, 0);
        let (res, buf_read) = timeout(READ_TIMEOUT, retval).await?;
        // Propagate errors, see how many bytes we read
        let n = res?;
        debug!("USB: after read, {} bytes", n);
        if n == 0 {
            // A read of size zero signals EOF (end of file), finish gracefully
            return Ok(());
        }

        // The `slice` method here is implemented in an extension trait: it
        // returns an owned slice of our `Vec<u8>`, which we later turn back
        // into the full `Vec<u8>`
        debug!("USB: before write");
        let (res, buf_write) = to.write(buf_read.slice(..n)).submit().await;
        let n = res?;
        debug!("USB: after write, {} bytes", n);
        // Increment byte counters for statistics
        if stats_interval.is_some() {
            bytes_out += n;
            bytes_out_last += n;
        }

        // Later is now, we want our full buffer back.
        // That's why we declared our binding `mut` way back at the start of `copy`,
        // even though we moved it into the very first `TcpStream::read` call.
        buf = buf_write.into_inner();
    }
}

async fn copy_stream_to_file(
    from: Rc<tokio_uring::net::TcpStream>,
    to: Rc<tokio_uring::fs::File>,
    stats_interval: Option<Duration>,
) -> Result<(), std::io::Error> {
    // For statistics
    let mut bytes_out: usize = 0;
    let mut bytes_out_last: usize = 0;
    let mut report_time = Instant::now();

    let mut buf = vec![0u8; BUFFER_LEN];
    loop {
        // Handle stats printing
        if stats_interval.is_some() && report_time.elapsed() > stats_interval.unwrap() {
            let transferred_total = ByteSize::b(bytes_out.try_into().unwrap());
            let transferred_last = ByteSize::b(bytes_out_last.try_into().unwrap());

            let speed: u64 =
                (bytes_out_last as f64 / report_time.elapsed().as_secs_f64()).round() as u64;
            let speed = ByteSize::b(speed);

            info!(
                "{} 📱 phone to car transfer: {:#} ({:#}/s), {:#} total",
                NAME,
                transferred_last.to_string_as(true),
                speed.to_string_as(true),
                transferred_total.to_string_as(true),
            );

            report_time = Instant::now();
            bytes_out_last = 0;
        }

        // things look weird: we pass ownership of the buffer to `read`, and we get
        // it back, _even if there was an error_. There's a whole trait for that,
        // which `Vec<u8>` implements!
        debug!("TCP: before read");
        let retval = from.read(buf);
        let (res, buf_read) = timeout(READ_TIMEOUT, retval).await?;
        // Propagate errors, see how many bytes we read
        let n = res?;
        debug!("TCP: after read, {} bytes", n);
        if n == 0 {
            // A read of size zero signals EOF (end of file), finish gracefully
            return Ok(());
        }

        // The `slice` method here is implemented in an extension trait: it
        // returns an owned slice of our `Vec<u8>`, which we later turn back
        // into the full `Vec<u8>`
        debug!("TCP: before write");
        let (res, buf_write) = to.write_at(buf_read.slice(..n), 0).submit().await;
        let n = res?;
        debug!("TCP: after write, {} bytes", n);
        // Increment byte counters for statistics
        if stats_interval.is_some() {
            bytes_out += n;
            bytes_out_last += n;
        }

        // Later is now, we want our full buffer back.
        // That's why we declared our binding `mut` way back at the start of `copy`,
        // even though we moved it into the very first `TcpStream::read` call.
        buf = buf_write.into_inner();
    }
}

pub async fn io_loop(
    stats_interval: Option<Duration>,
    need_restart: Arc<Notify>,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("{} 🛰️ Starting TCP server...", NAME);
    let bind_addr = format!("0.0.0.0:{}", TCP_SERVER_PORT).parse().unwrap();
    let listener = tokio_uring::net::TcpListener::bind(bind_addr).unwrap();
    info!(
        "{} 🛰️ TCP server now listening on: <u>{}</u>",
        NAME, bind_addr
    );
    loop {
        // Asynchronously wait for an inbound TCP connection
        let (stream, addr) = listener.accept().await?;
        info!(
            "{} 📳 TCP server: new client connected: <b>{:?}</b>",
            NAME, addr
        );

        info!(
            "{} 📂 Opening USB accessory device: <u>{}</u>",
            NAME, USB_ACCESSORY_PATH
        );
        use tokio_uring::fs::OpenOptions;
        let usb = OpenOptions::new()
            .read(true)
            .write(true)
            .create(false)
            .open(USB_ACCESSORY_PATH)
            .await?;

        info!(
            "{} ♾️ Starting to proxying data between TCP and USB...",
            NAME
        );

        // `read` and `write` take owned buffers (more on that later), and
        // there's no "per-socket" buffer, so they actually take `&self`.
        // which means we don't need to split them into a read half and a
        // write half like we'd normally do with "regular tokio". Instead,
        // we can send a reference-counted version of it. also, since a
        // tokio-uring runtime is single-threaded, we can use `Rc` instead of
        // `Arc`.
        let file = Rc::new(usb);
        let stream = Rc::new(stream);

        // We need to copy in both directions...
        let mut from_file = tokio_uring::spawn(copy_file_to_stream(
            file.clone(),
            stream.clone(),
            stats_interval,
        ));
        let mut from_stream = tokio_uring::spawn(copy_stream_to_file(
            stream.clone(),
            file.clone(),
            stats_interval,
        ));

        // Stop as soon as one of them errors
        let res = tokio::try_join!(&mut from_file, &mut from_stream);
        if let Err(e) = res {
            error!("{} Connection error: {}", NAME, e);
        }
        // Make sure the reference count drops to zero and the socket is
        // freed by aborting both tasks (which both hold a `Rc<TcpStream>`
        // for each direction)
        from_file.abort();
        from_stream.abort();

        // stream(s) closed, notify main loop to restart
        need_restart.notify_one();
    }
}
