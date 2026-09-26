// SPDX-License-Identifier: MPL-2.0

use notify::event::{ModifyKind, RenameMode};
use sctk::reexports::calloop::{LoopHandle, channel};
use std::collections::VecDeque;
use std::path::PathBuf;

use crate::CosmicBg;

/// Pure reducer for testing queue updates on filesystem events.
pub fn handle_event_on_queue(queue: &mut VecDeque<PathBuf>, event: &notify::Event) -> bool {
    let mut changed = false;
    match event.kind {
        notify::EventKind::Create(_)
        | notify::EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
            for p in &event.paths {
                if crate::wallpaper::is_image_candidate(p) && !queue.contains(p) {
                    queue.push_front(p.clone());
                    changed = true;
                }
            }
        }
        notify::EventKind::Remove(_)
        | notify::EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
            let initial_len = queue.len();
            queue.retain(|p| !event.paths.contains(p));
            if queue.len() != initial_len {
                changed = true;
            }
        }
        _ => {}
    }
    changed
}

pub fn img_source(handle: &LoopHandle<CosmicBg>) -> channel::SyncSender<(String, notify::Event)> {
    let (notify_tx, notify_rx) = channel::sync_channel(20);
    let _res = handle
        .insert_source(
            notify_rx,
            |e: channel::Event<(String, notify::Event)>, _, state| match e {
                channel::Event::Msg((source, event)) => {
                    for w in state
                        .wallpapers
                        .iter_mut()
                        .filter(|w| w.entry.output == source)
                    {
                        match event.kind {
                            notify::EventKind::Create(_)
                            | notify::EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                                if handle_event_on_queue(&mut w.image_queue, &event) {
                                    w.reconcile_timer();
                                    if w.current_source.is_none()
                                        && let Some(first) = w.image_queue.front()
                                    {
                                        w.current_source =
                                            Some(cosmic_bg_config::Source::Path(first.clone()));
                                        _ = w.save_state();
                                        w.mark_needs_redraw();
                                        w.draw(&mut state.image_cache);
                                    }
                                }
                            }
                            notify::EventKind::Remove(_)
                            | notify::EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                                for p in &event.paths {
                                    state.image_cache.invalidate(p);
                                }
                                if handle_event_on_queue(&mut w.image_queue, &event) {
                                    w.reconcile_timer();
                                    if let Some(cosmic_bg_config::Source::Path(ref cur_path)) =
                                        w.current_source
                                        && event.paths.contains(cur_path)
                                    {
                                        if let Some(next) = w.image_queue.front() {
                                            w.current_source =
                                                Some(cosmic_bg_config::Source::Path(next.clone()));
                                        } else {
                                            w.current_source = None;
                                        }
                                        _ = w.save_state();
                                        w.mark_needs_redraw();
                                        w.draw(&mut state.image_cache);
                                    }
                                }
                            }
                            notify::EventKind::Modify(_) => {
                                for p in &event.paths {
                                    state.image_cache.invalidate(p);
                                }
                                if let Some(cosmic_bg_config::Source::Path(ref cur_path)) =
                                    w.current_source
                                    && event.paths.contains(cur_path)
                                {
                                    w.mark_needs_redraw();
                                    w.draw(&mut state.image_cache);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                channel::Event::Closed => {
                    tracing::debug!("img_source channel closed");
                }
            },
        )
        .map(|_| {})
        .map_err(|err| eyre::eyre!("{}", err));

    notify_tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, RemoveKind};

    #[test]
    fn test_handle_event_create_retains_new_path() {
        let mut queue = VecDeque::from([PathBuf::from("/tmp/existing.png")]);
        let event = notify::Event {
            kind: notify::EventKind::Create(CreateKind::File),
            paths: vec![PathBuf::from("/tmp/added.png")],
            attrs: Default::default(),
        };

        let changed = handle_event_on_queue(&mut queue, &event);
        assert!(changed);
        assert_eq!(queue.len(), 2);
        assert!(queue.contains(&PathBuf::from("/tmp/added.png")));
        assert!(queue.contains(&PathBuf::from("/tmp/existing.png")));
    }

    #[test]
    fn test_handle_event_remove() {
        let mut queue = VecDeque::from([
            PathBuf::from("/tmp/img1.png"),
            PathBuf::from("/tmp/img2.png"),
        ]);
        let event = notify::Event {
            kind: notify::EventKind::Remove(RemoveKind::File),
            paths: vec![PathBuf::from("/tmp/img1.png")],
            attrs: Default::default(),
        };

        let changed = handle_event_on_queue(&mut queue, &event);
        assert!(changed);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0], PathBuf::from("/tmp/img2.png"));
    }

    #[test]
    fn test_handle_event_ignores_non_image() {
        let mut queue = VecDeque::new();
        let event = notify::Event {
            kind: notify::EventKind::Create(CreateKind::File),
            paths: vec![PathBuf::from("/tmp/readme.txt")],
            attrs: Default::default(),
        };

        let changed = handle_event_on_queue(&mut queue, &event);
        assert!(!changed);
        assert!(queue.is_empty());
    }
}
