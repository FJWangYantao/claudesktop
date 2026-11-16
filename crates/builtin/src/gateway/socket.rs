use std::io;
use std::net::TcpStream;
use std::os::windows::io::AsRawSocket;

const SOCKET_LEVEL: i32 = 0xffff;
const RECEIVE_BUFFER: i32 = 0x1002;
const RECEIVE_BYTES: i32 = 1 << 20;

#[link(name = "ws2_32")]
unsafe extern "system" {
    fn setsockopt(socket: usize, level: i32, name: i32, value: *const u8, length: i32) -> i32;
    fn WSAGetLastError() -> i32;
}

pub(super) fn configure(stream: &TcpStream) -> io::Result<()> {
    let size = RECEIVE_BYTES;
    let status = unsafe {
        setsockopt(
            stream.as_raw_socket() as usize,
            SOCKET_LEVEL,
            RECEIVE_BUFFER,
            (&size as *const i32).cast(),
            std::mem::size_of_val(&size) as i32,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[link(name = "ws2_32")]
    unsafe extern "system" {
        fn getsockopt(
            socket: usize,
            level: i32,
            name: i32,
            value: *mut u8,
            length: *mut i32,
        ) -> i32;
    }

    #[test]
    fn accepted_loopback_connection_has_room_for_large_requests() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        configure(&accepted).unwrap();
        let mut size = 0i32;
        let mut length = std::mem::size_of_val(&size) as i32;
        let status = unsafe {
            getsockopt(
                accepted.as_raw_socket() as usize,
                SOCKET_LEVEL,
                RECEIVE_BUFFER,
                (&mut size as *mut i32).cast(),
                &mut length,
            )
        };
        assert_eq!(status, 0);
        assert_eq!(length, std::mem::size_of_val(&size) as i32);
        assert!(size >= RECEIVE_BYTES);
    }
}
