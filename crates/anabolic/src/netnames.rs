// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Names for the remote addresses on screen: asked for as they are drawn,
//! looked up in the background, kept for the session.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;

use anabolic_core::resolver::{Answers, Resolver, local_kind};
use eframe::egui;

#[derive(Default)]
pub struct Names {
    /// Address -> the name a program looked up for it (from the DNS
    /// client's events); better than what the address says of itself.
    forward: Arc<HashMap<IpAddr, Arc<str>>>,
    resolver: Option<(Resolver, Answers)>,
    /// None: it has no name.
    known: HashMap<IpAddr, Option<Arc<str>>>,
    asked: HashSet<IpAddr>,
}

pub enum Name {
    /// A name from DNS.
    Dns(Arc<str>),
    /// What the address is without asking: this computer, the local network.
    Kind(&'static str),
    Looking,
    None,
}

impl Names {
    pub fn set_forward(&mut self, forward: Arc<HashMap<IpAddr, Arc<str>>>) {
        self.forward = forward;
    }

    /// The name of `ip`; asks for it the first time.
    pub fn get(&mut self, ctx: &egui::Context, ip: IpAddr) -> Name {
        if !ip.is_loopback()
            && let Some(name) = self.forward.get(&ip)
        {
            return Name::Dns(name.clone());
        }
        if let Some(kind) = local_kind(ip) {
            return Name::Kind(kind);
        }
        let (resolver, results) = self.resolver.get_or_insert_with(|| {
            let ctx = ctx.clone();
            Resolver::start(move || crate::stalls::wake(&ctx))
        });
        for (ip, name) in results.try_iter() {
            self.asked.remove(&ip);
            self.known.insert(ip, name);
        }
        match self.known.get(&ip) {
            Some(Some(name)) => Name::Dns(name.clone()),
            Some(None) => Name::None,
            None => {
                if self.asked.insert(ip) {
                    resolver.lookup(ip);
                }
                Name::Looking
            }
        }
    }
}
