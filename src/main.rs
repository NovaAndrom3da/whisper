mod pty;

use std::{error::Error, net::Ipv4Addr, process::abort};

use clap::Parser;
use ipstack::{IpStack, IpStackConfig};
use tokio::io::copy_bidirectional;
use tun2::{create_as_async, Configuration};
use wisp_mux::{ClientMux, StreamType};

/// Implementation of Wisp over a pty. Exposes the Wisp connection over a TUN device.
#[derive(Parser)]
#[command(version = clap::crate_version!())]
struct Cli {
    /// Path to PTY device
    #[arg(short, long)]
    pty: String,
    /// Name of created TUN device
    #[arg(short, long)]
    tun: String,
    /// MTU of created TUN device
    #[arg(short, long, default_value_t = u16::MAX)]
    mtu: u16,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn Error + 'static>> {
    let opts = Cli::parse();

    println!("Connecting to PTY: {:?}", opts.pty);
    let (rx, tx) = pty::open_pty(opts.pty).await?;
    let (mux, fut) = ClientMux::new(rx, tx).await?;

    tokio::spawn(async move {
        if let Err(err) = fut.await {
            eprintln!("Error in Wisp multiplexor future: {}", err);
            abort();
        }
    });

    println!("Creating TUN device with name: {:?}", opts.tun);
    let tun = create_as_async(
        Configuration::default()
            .address(Ipv4Addr::new(10, 0, 10, 2))
            .netmask(Ipv4Addr::new(255, 255, 255, 0))
            .destination(Ipv4Addr::new(10, 0, 10, 1))
            .platform_config(|c| {
                c.ensure_root_privileges(true);
            })
            .mtu(opts.mtu)
            .tun_name(opts.tun)
            .up(),
    )?;

    let mut ip_stack_config = IpStackConfig::default();
    ip_stack_config.mtu(opts.mtu);
    let mut ip_stack = IpStack::new(ip_stack_config, tun);

    loop {
        use ipstack::stream::IpStackStream as S;
        match ip_stack.accept().await? {
            S::Tcp(mut tcp) => {
                let addr = tcp.peer_addr();
                let mut stream = mux
                    .client_new_stream(StreamType::Tcp, addr.ip().to_string(), addr.port())
                    .await?
                    .into_io()
                    .into_asyncrw();
                tokio::spawn(async move {
                    if let Err(err) = copy_bidirectional(&mut tcp, &mut stream).await {
                        eprintln!("Error while forwarding TCP stream: {}", err);
                    }
                });
            }
            S::Udp(mut udp) => {
                let addr = udp.peer_addr();
                let mut stream = mux
                    .client_new_stream(StreamType::Udp, addr.ip().to_string(), addr.port())
                    .await?
                    .into_io()
                    .into_asyncrw();
                tokio::spawn(async move {
                    if let Err(err) = copy_bidirectional(&mut udp, &mut stream).await {
                        eprintln!("Error while forwarding UDP datagrams: {}", err);
                    }
                });
            }
            _ => {}
        }
    }
}
