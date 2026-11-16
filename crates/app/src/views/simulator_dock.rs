//! The simulator tab: act_devices' panel mounted in the dock, streaming only while it is on screen
//! (SimulatorPanel.tsx, SimulatorDeviceMenu.tsx).

use std::rc::Rc;

use act_core::state::{front_dock, WorkspaceState};
use act_devices::simulator::SimulatorPanel;
use act_ui::theme::ActiveTheme;
use gpui::{div, prelude::*, AnyElement, Context, Entity, Window};

use super::root::Root;
use crate::workspace::dispatch;

#[derive(Default)]
pub struct SimulatorDock {
    panel: Option<Entity<SimulatorPanel>>,
}

impl SimulatorDock {
    /// Feeds the panel the devices and this dock's device; it streams only while `active`.
    pub fn sync(&mut self, state: &WorkspaceState, active: bool, window: &mut Window, cx: &mut Context<Root>) {
        if self.panel.is_none() {
            if !active {
                return;
            }
            let Some(host) = crate::workspace::workspace(cx).read(cx).simulator.clone() else { return };
            let panel = cx.new(|cx| {
                SimulatorPanel::new(
                    host,
                    Rc::new(|command, _, cx| {
                        let command = command.clone();
                        cx.defer(move |cx| dispatch(command, cx));
                    }),
                    window,
                    cx,
                )
            });
            self.panel = Some(panel);
        }
        let (_, dock) = front_dock(state);
        let simulator = state.simulator.clone();
        let udid = dock.simulator_id.clone();
        if let Some(panel) = &self.panel {
            panel.update(cx, |panel, cx| panel.set(simulator, udid, active, cx));
        }
    }

    pub fn hide(&mut self, state: &WorkspaceState, cx: &mut Context<Root>) {
        if let Some(panel) = &self.panel {
            let (_, dock) = front_dock(state);
            let (simulator, udid) = (state.simulator.clone(), dock.simulator_id.clone());
            panel.update(cx, |panel, cx| panel.set(simulator, udid, false, cx));
        }
    }

    pub fn render(&self, cx: &mut Context<Root>) -> AnyElement {
        match &self.panel {
            Some(panel) => div().size_full().child(panel.clone()).into_any_element(),
            None => {
                let sunken = cx.theme().tokens.sunken;
                div().size_full().bg(sunken).into_any_element()
            }
        }
    }
}
