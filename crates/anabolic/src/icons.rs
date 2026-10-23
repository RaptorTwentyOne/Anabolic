// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! GPU textures for process icons, uploaded once per distinct image.

use std::collections::HashMap;

use anabolic_core::Icon;
use eframe::egui::{self, ColorImage, TextureHandle, TextureId, TextureOptions};

#[derive(Default)]
pub struct IconCache {
    textures: HashMap<u64, TextureHandle>,
}

impl IconCache {
    pub fn texture(&mut self, ctx: &egui::Context, icon: &Icon) -> TextureId {
        self.textures
            .entry(icon.id)
            .or_insert_with(|| {
                let image = ColorImage::from_rgba_unmultiplied(
                    [icon.width as usize, icon.height as usize],
                    &icon.rgba,
                );
                ctx.load_texture(format!("icon-{}", icon.id), image, TextureOptions::LINEAR)
            })
            .id()
    }

    /// Drops textures no longer referenced by any live process.
    pub fn retain(&mut self, mut keep: impl FnMut(u64) -> bool) {
        self.textures.retain(|id, _| keep(*id));
    }
}
