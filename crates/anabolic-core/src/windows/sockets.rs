// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Every TCP and UDP endpoint on the machine and the process that owns it,
//! from Windows' own tables (what `netstat -ano` lists): connections in any
//! state, and ports waiting for others to connect. Read from the IP helper;
//! no process is opened. Also closing a TCP connection from outside.

use std::net::{IpAddr, Ipv4Addr};

use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCP_STATE_DELETE_TCB, MIB_TCP6ROW_OWNER_PID,
    MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_LH, MIB_TCPROW_LH_0, MIB_TCPROW_OWNER_PID,
    MIB_TCPTABLE_OWNER_PID, MIB_UDP6ROW_OWNER_PID, MIB_UDP6TABLE_OWNER_PID, MIB_UDPROW_OWNER_PID,
    MIB_UDPTABLE_OWNER_PID, SetTcpEntry, TCP_TABLE_OWNER_PID_ALL, UDP_TABLE_OWNER_PID,
};
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};

use crate::nettrace::Proto;

use super::*;

impl TcpState {
    fn from_raw(state: u32) -> Option<Self> {
        Some(match state {
            1 => TcpState::Closed,
            2 => TcpState::Listen,
            3 => TcpState::SynSent,
            4 => TcpState::SynReceived,
            5 => TcpState::Established,
            6 => TcpState::FinWait1,
            7 => TcpState::FinWait2,
            8 => TcpState::CloseWait,
            9 => TcpState::Closing,
            10 => TcpState::LastAck,
            11 => TcpState::TimeWait,
            12 => TcpState::DeleteTcb,
            _ => return None,
        })
    }
}

/// Ports sit in network order in the low word.
fn port(p: u32) -> u16 {
    u16::from_be(p as u16)
}

/// Runs a "how big is the table" call until the buffer holds it (it can
/// grow between two calls).
fn table(
    call: impl Fn(Option<*mut core::ffi::c_void>, &mut u32) -> u32,
) -> Result<Vec<u64>, String> {
    let mut size = 0u32;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..5 {
        let ptr = (!buf.is_empty()).then(|| buf.as_mut_ptr().cast());
        let err = call(ptr, &mut size);
        if err == 0 && !buf.is_empty() {
            return Ok(buf);
        }
        // ERROR_INSUFFICIENT_BUFFER: size says how much; anything else fails.
        if err != 0 && err != 122 {
            return Err(crate::util::error_text(&windows::core::Error::from(
                windows::Win32::Foundation::WIN32_ERROR(err).to_hresult(),
            )));
        }
        buf = vec![0; (size as usize).div_ceil(8) + 128];
    }
    Err("the table kept growing".into())
}

fn tcp4() -> Result<Vec<Socket>, String> {
    let buf = table(|p, size| unsafe {
        GetExtendedTcpTable(p, size, false, AF_INET.0 as u32, TCP_TABLE_OWNER_PID_ALL, 0)
    })?;
    let table = unsafe { &*buf.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>() };
    let rows: &[MIB_TCPROW_OWNER_PID] =
        unsafe { std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize) };
    Ok(rows
        .iter()
        .map(|r| {
            let state = TcpState::from_raw(r.dwState);
            Socket {
                pid: r.dwOwningPid,
                proto: Proto::Tcp,
                local: IpAddr::V4(Ipv4Addr::from(r.dwLocalAddr.to_ne_bytes())),
                local_port: port(r.dwLocalPort),
                remote: (state != Some(TcpState::Listen)).then(|| {
                    (
                        IpAddr::V4(Ipv4Addr::from(r.dwRemoteAddr.to_ne_bytes())),
                        port(r.dwRemotePort),
                    )
                }),
                state,
            }
        })
        .collect())
}

fn tcp6() -> Result<Vec<Socket>, String> {
    let buf = table(|p, size| unsafe {
        GetExtendedTcpTable(
            p,
            size,
            false,
            AF_INET6.0 as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        )
    })?;
    let table = unsafe { &*buf.as_ptr().cast::<MIB_TCP6TABLE_OWNER_PID>() };
    let rows: &[MIB_TCP6ROW_OWNER_PID] =
        unsafe { std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize) };
    Ok(rows
        .iter()
        .map(|r| {
            let state = TcpState::from_raw(r.dwState);
            Socket {
                pid: r.dwOwningPid,
                proto: Proto::Tcp,
                local: ip6(r.ucLocalAddr),
                local_port: port(r.dwLocalPort),
                remote: (state != Some(TcpState::Listen))
                    .then(|| (ip6(r.ucRemoteAddr), port(r.dwRemotePort))),
                state,
            }
        })
        .collect())
}

fn udp4() -> Result<Vec<Socket>, String> {
    let buf = table(|p, size| unsafe {
        GetExtendedUdpTable(p, size, false, AF_INET.0 as u32, UDP_TABLE_OWNER_PID, 0)
    })?;
    let table = unsafe { &*buf.as_ptr().cast::<MIB_UDPTABLE_OWNER_PID>() };
    let rows: &[MIB_UDPROW_OWNER_PID] =
        unsafe { std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize) };
    Ok(rows
        .iter()
        .map(|r| Socket {
            pid: r.dwOwningPid,
            proto: Proto::Udp,
            local: IpAddr::V4(Ipv4Addr::from(r.dwLocalAddr.to_ne_bytes())),
            local_port: port(r.dwLocalPort),
            remote: None,
            state: None,
        })
        .collect())
}

fn udp6() -> Result<Vec<Socket>, String> {
    let buf = table(|p, size| unsafe {
        GetExtendedUdpTable(p, size, false, AF_INET6.0 as u32, UDP_TABLE_OWNER_PID, 0)
    })?;
    let table = unsafe { &*buf.as_ptr().cast::<MIB_UDP6TABLE_OWNER_PID>() };
    let rows: &[MIB_UDP6ROW_OWNER_PID] =
        unsafe { std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize) };
    Ok(rows
        .iter()
        .map(|r| Socket {
            pid: r.dwOwningPid,
            proto: Proto::Udp,
            local: ip6(r.ucLocalAddr),
            local_port: port(r.dwLocalPort),
            remote: None,
            state: None,
        })
        .collect())
}

/// Every TCP and UDP endpoint, IPv4 and IPv6. A table that cannot be read
/// is reported and the others still come back.
pub fn sockets() -> (Vec<Socket>, Option<String>) {
    let mut all = Vec::new();
    let mut errors = Vec::new();
    for (what, read) in [
        ("TCP", tcp4 as fn() -> Result<Vec<Socket>, String>),
        ("TCP over IPv6", tcp6),
        ("UDP", udp4),
        ("UDP over IPv6", udp6),
    ] {
        match read() {
            Ok(mut list) => all.append(&mut list),
            Err(e) => errors.push(format!("{what}: {e}")),
        }
    }
    let error = (!errors.is_empty()).then(|| errors.join("; "));
    (all, error)
}

/// Ends one TCP connection of a process, as TCPView's "Close connection"
/// does: the TCP stack drops it (the program sees a reset); the process is
/// not opened. IPv4 only: Windows offers no way to do this for IPv6. Needs
/// administrator rights.
pub fn close_tcp(
    pid: u32,
    remote: Ipv4Addr,
    remote_port: u16,
    local_port: u16,
) -> Result<(), String> {
    let buf = table(|p, size| unsafe {
        GetExtendedTcpTable(p, size, false, AF_INET.0 as u32, TCP_TABLE_OWNER_PID_ALL, 0)
    })?;
    let table = unsafe { &*buf.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>() };
    let rows: &[MIB_TCPROW_OWNER_PID] =
        unsafe { std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize) };
    let row = rows
        .iter()
        .find(|r| {
            r.dwOwningPid == pid
                && port(r.dwLocalPort) == local_port
                && port(r.dwRemotePort) == remote_port
                && Ipv4Addr::from(r.dwRemoteAddr.to_ne_bytes()) == remote
        })
        .ok_or("the connection is closed already")?;
    let delete = MIB_TCPROW_LH {
        Anonymous: MIB_TCPROW_LH_0 {
            State: MIB_TCP_STATE_DELETE_TCB,
        },
        dwLocalAddr: row.dwLocalAddr,
        dwLocalPort: row.dwLocalPort,
        dwRemoteAddr: row.dwRemoteAddr,
        dwRemotePort: row.dwRemotePort,
    };
    match unsafe { SetTcpEntry(&delete) } {
        0 => Ok(()),
        err => Err(crate::util::error_text(&windows::core::Error::from(
            windows::Win32::Foundation::WIN32_ERROR(err).to_hresult(),
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn sees_its_own_listener_and_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let _client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_server, _) = listener.accept().unwrap();
        let (all, error) = sockets();
        assert!(error.is_none(), "{error:?}");
        let me = std::process::id();
        assert!(all.iter().any(|s| s.pid == me
            && s.local_port == port
            && s.state == Some(TcpState::Listen)
            && s.listening()));
        assert!(all.iter().any(|s| s.pid == me
            && s.state == Some(TcpState::Established)
            && s.remote == Some((IpAddr::V4(Ipv4Addr::LOCALHOST), port))));
    }
}
