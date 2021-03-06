// Copyright 2017 Google Inc. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Handles syntax highlighting and other styling.
//!
//! Plugins provide syntax highlighting information in the form of 'scopes'.
//! Scope information originating from any number of plugins can be resolved
//! into styles using a theme, augmented with additional style definitions.

use std::collections::BTreeMap;
use syntect::parsing::Scope;

use xi_rope::interval::Interval;
use xi_rope::spans::{Spans, SpansBuilder};

use tabs::DocumentCtx;
use styles::Style;
use plugins::PluginPid;

/// A collection of layers containing scope information.
#[derive(Default)]
//TODO: rename. Probably to `Layers`
pub struct Scopes {
    layers: BTreeMap<PluginPid, ScopeLayer>,
    merged: Spans<Style>,
}

/// A collection of scope spans from a single source.
pub struct ScopeLayer {
    stack_lookup: Vec<Vec<Scope>>,
    style_lookup: Vec<Style>,
    /// Human readable scope names, for debugging
    name_lookup: Vec<Vec<String>>,
    scope_spans: Spans<u32>,
    style_spans: Spans<Style>,
}

impl Scopes {

    pub fn get_merged(&self) -> &Spans<Style> {
        &self.merged
    }

    /// Adds the provided scopes to the layer's lookup table.
    pub fn add_scopes(&mut self, layer: PluginPid, scopes: Vec<Vec<String>>,
                                doc_ctx: &DocumentCtx) {
        self.create_if_missing(layer);
        self.layers.get_mut(&layer).unwrap().add_scopes(scopes, doc_ctx);
    }

    /// Inserts empty spans at the given interval for all layers.
    ///
    /// This is useful for clearing spans, and for updating spans
    /// as edits occur.
    pub fn update_all(&mut self, iv: Interval, len: usize) {
        self.merged.edit(iv, SpansBuilder::new(len).build());
        let empty_spans = SpansBuilder::new(len).build();
        for layer in self.layers.values_mut() {
            layer.update_scopes(iv, &empty_spans);
        }
        self.resolve_styles(iv);
    }

    /// Updates the scope spans for a given layer.
    pub fn update_layer(&mut self, layer: PluginPid, iv: Interval, spans: Spans<u32>) {
        self.create_if_missing(layer);
        self.layers.get_mut(&layer).unwrap().update_scopes(iv, &spans);
        self.resolve_styles(iv);
    }

    /// Removes a given layer. This will remove all styles derived from
    /// that layer's scopes.
    pub fn remove_layer(&mut self, layer: PluginPid) -> Option<ScopeLayer> {
        let layer = self.layers.remove(&layer);
        if layer.is_some() {
            let iv_all = Interval::new_closed_closed(0, self.merged.len());
            //TODO: should Spans<T> have a clear() method?
            self.merged = SpansBuilder::new(self.merged.len()).build();
            self.resolve_styles(iv_all);
        }
        layer
    }

    pub fn theme_changed(&mut self, doc_ctx: &DocumentCtx) {
        for layer in self.layers.values_mut() {
            layer.theme_changed(doc_ctx);
        }
        self.merged = SpansBuilder::new(self.merged.len()).build();
        let iv_all = Interval::new_closed_closed(0, self.merged.len());
        self.resolve_styles(iv_all);
    }

    /// Resolves styles from all layers for the given interval, updating
    /// the master style spans.
    fn resolve_styles(&mut self, iv: Interval) {
        if self.layers.is_empty() {
            return
        }
        let mut layer_iter = self.layers.values();
        let mut resolved = layer_iter.next().unwrap().style_spans.subseq(iv);

        for other in layer_iter {
            let spans = other.style_spans.subseq(iv);
            assert_eq!(resolved.len(), spans.len());
            resolved = resolved.merge(&spans, |a, b| {
                match b {
                    Some(b) => a.merge(b),
                    None => a.to_owned(),
                }
            });
        }
        self.merged.edit(iv, resolved);
    }

    /// Prints scopes and style information for the given `Interval`.
    pub fn debug_print_spans(&self, iv: Interval) {
        for (id, layer) in self.layers.iter() {
            let spans = layer.scope_spans.subseq(iv);
            let styles = layer.style_spans.subseq(iv);
            if spans.iter().next().is_some() {
                print_err!("scopes for layer {:?}:", id);
                for (iv, val) in spans.iter() {
                    print_err!("{}: {:?}", iv, layer.name_lookup[*val as usize]);
                }
                print_err!("styles:");
                for (iv, val) in styles.iter() {
                    print_err!("{}: {:?}", iv, val);
                }
            }
        }
    }


    fn create_if_missing(&mut self, layer_id: PluginPid) {
        if !self.layers.contains_key(&layer_id) {
            self.layers.insert(layer_id, ScopeLayer::new(self.merged.len()));
        }
    }
}

impl Default for ScopeLayer {
    fn default() -> Self {
        ScopeLayer {
            stack_lookup: Vec::new(),
            style_lookup: Vec::new(),
            name_lookup: Vec::new(),
            scope_spans: Spans::default(),
            style_spans: Spans::default(),
        }
    }
}

impl ScopeLayer {

    pub fn new(len: usize) -> Self {
        ScopeLayer {
            stack_lookup: Vec::new(),
            style_lookup: Vec::new(),
            name_lookup: Vec::new(),
            scope_spans: SpansBuilder::new(len).build(),
            style_spans: SpansBuilder::new(len).build(),
        }
    }

    fn theme_changed(&mut self, doc_ctx: &DocumentCtx) {
        // recompute styles with the new theme
        self.style_lookup = self.styles_for_stacks(self.stack_lookup.as_slice(), doc_ctx);
        let iv_all = Interval::new_closed_closed(0, self.style_spans.len());
        self.style_spans = SpansBuilder::new(self.style_spans.len()).build();
        // this feels unnecessary but we can't pass in a reference to self
        // and I don't want to get fancy unless there's an actual perf problem
        let scopes = self.scope_spans.clone();
        self.update_styles(iv_all, &scopes)
    }

    fn add_scopes(&mut self, scopes: Vec<Vec<String>>,
                                doc_ctx: &DocumentCtx) {
        let mut stacks = Vec::with_capacity(scopes.len());
        for stack in scopes {
            let scopes = stack.iter().map(|s| Scope::new(&s))
                .filter(|result| match *result {
                    Err(ref err) => {
                        print_err!("failed to resolve scope {}\nErr: {:?}",
                                   &stack.join(" "),
                                   err);
                        false
                    }
                    _ => true
                })
                .map(|s| s.unwrap())
                .collect::<Vec<_>>();
            stacks.push(scopes);
            self.name_lookup.push(stack);
        }

        let mut new_styles = self.styles_for_stacks(stacks.as_slice(), doc_ctx);
        self.stack_lookup.append(&mut stacks);
        self.style_lookup.append(&mut new_styles);
    }

    fn styles_for_stacks(&self, stacks: &[Vec<Scope>],
                         doc_ctx: &DocumentCtx) -> Vec<Style> {
        let style_map = doc_ctx.get_style_map().lock().unwrap();
        let highlighter = style_map.get_highlighter();

        let mut new_styles = Vec::new();
        for stack in stacks {
            let style = highlighter.style_mod_for_stack(stack);
            let style = Style::from_syntect_style_mod(&style);
            new_styles.push(style);
        }
        new_styles
    }

    fn update_scopes(&mut self, iv: Interval, spans: &Spans<u32>) {
        self.scope_spans.edit(iv, spans.to_owned());
        self.update_styles(iv, spans);
    }

    /// Updates `self.style_spans`, mapping scopes to styles and combining
    /// adjacent and equal spans.
    fn update_styles(&mut self, iv: Interval, spans: &Spans<u32>) {

        // NOTE: This is a tradeoff. Keeping both u32 and Style spans for each
        // layer makes debugging simpler and reduces the total number of spans
        // on the wire (because we combine spans that resolve to the same style)
        // but it does require additional computation + memory up front.
        let mut sb = SpansBuilder::new(spans.len());
        let mut spans_iter = spans.iter();
        let mut prev = spans_iter.next();
        {
        // distinct adjacent scopes can often resolve to the same style,
        // so we combine them when building the styles.
        let style_eq = |i1: &u32, i2: &u32| {
            self.style_lookup[*i1 as usize] == self.style_lookup[*i2 as usize]
        };

        while let Some((p_iv, p_val)) = prev {
            match spans_iter.next() {
                Some((n_iv, n_val)) if n_iv.start() == p_iv.end() && style_eq(p_val, n_val) => {
                    prev = Some((p_iv.union(n_iv), p_val));
                }
                other => {
                    sb.add_span(p_iv, self.style_lookup[*p_val as usize].to_owned());
                    prev = other;
                }
            }
        }
        }
        self.style_spans.edit(iv, sb.build());
    }
}
