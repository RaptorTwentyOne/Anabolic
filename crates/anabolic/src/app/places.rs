//! Back and forward, as in a browser: Alt+← and Alt+→, or the mouse's back
//! and forward buttons. A place is a page or a Performance view, with the
//! process selected and its details' tab on the Processes page and the
//! service selected on Services. Going to another page or view is a step,
//! and so is following a link to a process or a service on the same page;
//! picking another row of a list is not.

use super::*;

/// Steps kept behind.
const KEPT: usize = 100;

#[derive(Clone, PartialEq)]
pub(super) struct Place {
    tab: Tab,
    perf_view: PerfView,
    selected: Option<ProcKey>,
    detail_tab: DetailTab,
    service: Option<Arc<str>>,
}

impl Place {
    /// The page and view: what a step changes.
    fn page(&self) -> (Tab, Option<PerfView>) {
        (
            self.tab,
            (self.tab == Tab::Performance).then_some(self.perf_view),
        )
    }
}

#[derive(Default)]
pub(super) struct Places {
    back: Vec<Place>,
    forward: Vec<Place>,
    /// Where the last frame ended.
    here: Option<Place>,
    /// A link was followed this frame: a step even on the same page.
    jumped: bool,
}

impl Places {
    /// Where a frame ended: a new page or a followed link left a place
    /// behind.
    fn note(&mut self, now: Place) {
        let jumped = std::mem::take(&mut self.jumped);
        if let Some(before) = self.here.take()
            && (jumped || before.page() != now.page())
            && before != now
        {
            self.back.push(before);
            if self.back.len() > KEPT {
                self.back.remove(0);
            }
            self.forward.clear();
        }
        self.here = Some(now);
    }

    /// The place one step back, or forward, from `left`.
    fn step(&mut self, forward: bool, left: Place) -> Option<Place> {
        let (from, to) = if forward {
            (&mut self.forward, &mut self.back)
        } else {
            (&mut self.back, &mut self.forward)
        };
        let place = from.pop()?;
        to.push(left);
        Some(place)
    }

    /// Arrived where a step led (or as near as could be): not a step itself.
    fn arrived(&mut self, here: Place) {
        self.here = Some(here);
        self.jumped = false;
    }
}

impl App {
    fn place(&self) -> Place {
        Place {
            tab: self.settings.tab,
            perf_view: self.perf_view,
            selected: self.table.selected,
            detail_tab: self.detail_tab,
            service: self.services.selected.clone(),
        }
    }

    /// A link to a process or a service was followed.
    pub(super) fn jumped(&mut self) {
        self.places.jumped = true;
    }

    /// At the end of a frame.
    pub(super) fn note_place(&mut self) {
        let now = self.place();
        self.places.note(now);
    }

    /// One step back, or forward.
    pub(super) fn step(&mut self, forward: bool) {
        let left = self.place();
        let Some(place) = self.places.step(forward, left) else {
            return;
        };
        self.settings.tab = place.tab;
        // Each page keeps its own state but where it was left.
        match place.tab {
            Tab::Performance => self.perf_view = place.perf_view,
            Tab::Processes => {
                // A process that has gone since is not looked for.
                match place.selected {
                    Some(key) if self.snapshot.find(key).is_some() => self.go_to(key),
                    Some(_) => {}
                    None => self.select(None),
                }
                self.detail_tab = place.detail_tab;
            }
            Tab::Services => self.services.selected = place.service,
            _ => {}
        }
        let here = self.place();
        self.places.arrived(here);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(tab: Tab) -> Place {
        Place {
            tab,
            perf_view: PerfView::default(),
            selected: None,
            detail_tab: DetailTab::General,
            service: None,
        }
    }

    fn perf(view: PerfView) -> Place {
        Place {
            perf_view: view,
            ..at(Tab::Performance)
        }
    }

    #[test]
    fn steps_back_and_forward_through_pages() {
        let mut places = Places::default();
        places.note(at(Tab::Processes));
        places.note(at(Tab::Network));
        places.note(perf(PerfView::Disk));
        // The same view, frame after frame: no step.
        places.note(perf(PerfView::Disk));
        places.note(perf(PerfView::Sensors));

        let back = places.step(false, perf(PerfView::Sensors)).unwrap();
        assert!(back == perf(PerfView::Disk));
        places.arrived(back.clone());
        let back = places.step(false, back).unwrap();
        assert!(back == at(Tab::Network));
        places.arrived(back.clone());
        let forward = places.step(true, back).unwrap();
        assert!(forward == perf(PerfView::Disk));
        places.arrived(forward.clone());

        // A new page from here drops what lay ahead.
        places.note(at(Tab::Services));
        assert!(places.step(true, at(Tab::Services)).is_none());
        assert!(places.step(false, at(Tab::Services)).unwrap() == perf(PerfView::Disk));
    }

    #[test]
    fn a_link_is_a_step_and_picking_a_row_is_not() {
        let key = |pid| ProcKey {
            pid,
            create_time: 1,
        };
        let on = |pid| Place {
            selected: Some(key(pid)),
            ..at(Tab::Processes)
        };
        let mut places = Places::default();
        places.note(on(10));
        // Picking rows: the place moves along, no step is left.
        places.note(on(11));
        places.note(on(12));
        // A link to the parent, on the same page: a step.
        places.jumped = true;
        places.note(on(1));
        let back = places.step(false, on(1)).unwrap();
        assert!(back == on(12));
        // Nothing before the first place.
        places.arrived(back.clone());
        assert!(places.step(false, back).is_none());
    }

    #[test]
    fn keeps_so_many_steps() {
        let mut places = Places::default();
        for i in 0..KEPT + 20 {
            places.note(if i % 2 == 0 {
                at(Tab::Processes)
            } else {
                at(Tab::Network)
            });
        }
        assert_eq!(places.back.len(), KEPT);
    }
}
