// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use netbench::{multiplex, scenario, Result, Timer};
use netbench_driver::Allocator;
use std::{collections::HashSet, sync::Arc};
use structopt::StructOpt;
use tokio::{
    io::{self, AsyncReadExt},
    net::{TcpListener, TcpStream},
    spawn,
};
use tokio_native_tls::native_tls::{Identity, TlsAcceptor};

#[global_allocator]
static ALLOCATOR: Allocator = Allocator::new();

fn main() -> Result<()> {
    let args = Server::from_args();
    let runtime = args.opts.runtime();
    runtime.block_on(args.run())
}

#[derive(Debug, StructOpt)]
pub struct Server {
    #[structopt(flatten)]
    opts: netbench_driver::Server,
}

impl Server {
    pub async fn run(&self) -> Result<()> {
        let scenario = self.opts.scenario();
        let buffer = (*self.opts.rx_buffer as usize, *self.opts.tx_buffer as usize);

        let server = self.server().await?;

        let trace = self.opts.trace();
        let config = self.opts.multiplex();
        let ident = self.identity()?;
        let acceptor = TlsAcceptor::builder(ident).build()?;
        let acceptor: tokio_native_tls::TlsAcceptor = acceptor.into();
        let acceptor = Arc::new(acceptor);

        let mut conn_id = 0;
        loop {
            let (connection, _addr) = server.accept().await?;

            if !self.opts.nagle {
                let _ = connection.set_nodelay(true);
            }

            let scenario = scenario.clone();
            let id = conn_id;
            conn_id += 1;
            let acceptor = acceptor.clone();
            let trace = trace.clone();
            let config = config.clone();
            spawn(async move {
                if let Err(err) =
                    handle_connection(acceptor, connection, id, scenario, trace, config, buffer)
                        .await
                {
                    eprintln!("error: {err}");
                }
            });
        }

        async fn handle_connection(
            acceptor: Arc<tokio_native_tls::TlsAcceptor>,
            connection: TcpStream,
            conn_id: u64,
            scenario: Arc<scenario::Server>,
            mut trace: impl netbench::Trace,
            config: Option<multiplex::Config>,
            (rx_buffer, tx_buffer): (usize, usize),
        ) -> Result<()> {
            let connection = io::BufStream::with_capacity(rx_buffer, tx_buffer, connection);

            let mut timer = netbench::timer::Tokio::default();
            let before = timer.now();

            let connection = acceptor.accept(connection).await?;

            let now = timer.now();
            trace.connect(now, conn_id, now - before);

            let mut connection = Box::pin(connection);

            let server_idx = connection.read_u64().await?;
            let scenario = scenario
                .connections
                .get(server_idx as usize)
                .ok_or("invalid connection id")?;

            let mut checkpoints = HashSet::new();

            if let Some(config) = config {
                let conn = netbench::multiplex::Connection::new(conn_id, connection, config);
                let conn = netbench::Driver::new(scenario, conn);
                conn.run(&mut trace, &mut checkpoints, &mut timer).await?;
            } else {
                let conn = netbench::duplex::Connection::new(conn_id, connection);
                let conn = netbench::Driver::new(scenario, conn);
                conn.run(&mut trace, &mut checkpoints, &mut timer).await?;
            }

            Ok(())
        }
    }

    async fn server(&self) -> Result<TcpListener> {
        let server = TcpListener::bind((self.opts.ip, self.opts.port)).await?;
        Ok(server)
    }

    fn identity(&self) -> Result<Identity> {
        let (_, private_key) = self.opts.certificate();
        let cert = &private_key.pkcs12;
        let ident = Identity::from_pkcs12(cert, "")?;
        Ok(ident)
    }
}
