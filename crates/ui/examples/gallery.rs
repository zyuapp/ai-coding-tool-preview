//! Every primitive on one page, for checking the design system against the Electron app.
//!
//! `cargo run -p act_ui --example gallery [theme-id] [--dialog] [--menu]`

use act_ui::components::button::{
    Button, ButtonSize, ButtonVariant, FillTone, IconButton, IconButtonSize, IconButtonVariant, Segmented, SegmentedOption,
    SegmentedVariant, ToggleTone,
};
use act_ui::components::chip::Chip;
use act_ui::components::confirm_action::ConfirmAction;
use act_ui::components::dialog::{dialog_paragraph, Dialog};
use act_ui::components::empty_state::EmptyState;
use act_ui::components::field::{Checkbox, FieldGroup, SearchField, SearchVariant, Switch, TextField};
use act_ui::components::kbd::Kbd;
use act_ui::components::list::{ListHeading, ListRow};
use act_ui::components::menu::{Menu, MenuEntry, MenuItem, MenuTrigger, PopoverMenu, TriggerVariant};
use act_ui::components::meter::{Meter, MeterSize};
use act_ui::components::nav_list::{nav_list, NavItem, NavSeparator};
use act_ui::components::notice::{Notice, NoticeTone};
use act_ui::components::panel_header::{toolbar, PanelHeader};
use act_ui::components::popover::Popover;
use act_ui::components::scrollbar::Scrollbar;
use act_ui::components::select::{Select, SelectOption};
use act_ui::components::spinner::Spinning;
use act_ui::components::surface::{Surface, SurfaceKind};
use act_ui::components::toast::{ToastCard, ToastTone, Toasts};
use act_ui::input::{Autosize, TextInput};
use act_ui::overlay::Placement;
use act_ui::theme::{ActiveTheme, Theme, ThemeMode, Tone, THEMES};
use act_ui::tooltip::tooltip_layer;
use act_ui::{Assets, Icon, IconName};
use gpui::{
    div, prelude::*, px, size, App, Bounds, Context, DismissEvent, Entity, FontWeight, MouseButton,
    ScrollHandle, SharedString, Subscription, Window, WindowBounds, WindowOptions,
};

struct Gallery {
    segment: SharedString,
    pill: SharedString,
    nav: usize,
    switch_on: bool,
    checked: bool,
    select: SharedString,
    search: Entity<TextInput>,
    name: Entity<TextInput>,
    composer: Entity<TextInput>,
    context_menu: Option<(Entity<Menu>, Subscription)>,
    dialog: bool,
    toasts: Vec<ToastCard>,
    list_scroll: ScrollHandle,
    folded: bool,
}

fn section(title: &'static str, cx: &App) -> gpui::Div {
    let t = &cx.theme().tokens;
    div()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(div().text_size(px(12.0)).font_weight(FontWeight(600.0)).text_color(t.quiet).child(title))
}

fn row() -> gpui::Div {
    div().flex().flex_wrap().items_center().gap(px(8.0))
}

fn sample_menu() -> Vec<MenuEntry> {
    vec![
        MenuItem::new("Rename").icon(IconName::Pencil).shortcut("⌘R").into(),
        MenuItem::new("Fork thread").icon(IconName::GitFork).into(),
        MenuItem::new("Move to worktree")
            .icon(IconName::FolderGit2)
            .submenu(vec![
                MenuItem::new("New worktree").icon(IconName::Plus).into(),
                MenuEntry::Separator,
                MenuItem::new("feature-auth").checked(true).into(),
                MenuItem::new("fix-scroll").checked(false).into(),
            ])
            .into(),
        MenuEntry::Group {
            label: "Open in".into(),
            items: vec![
                MenuItem::new("Terminal").icon(IconName::SquareTerminal).detail("on mac-mini"),
                MenuItem::new("Finder").icon(IconName::FolderOpen).description("Reveal the checkout"),
                MenuItem::new("Simulator").icon(IconName::Smartphone).disabled(true).tip("Not on remote").detail("Not on remote"),
            ],
        },
        MenuEntry::Separator,
        MenuItem::new("Archive").icon(IconName::Archive).danger(true).into(),
    ]
}

impl Gallery {
    fn open_context_menu(&mut self, position: gpui::Point<gpui::Pixels>, searchable: bool, window: &mut Window, cx: &mut Context<Self>) {
        let menu = Menu::build(sample_menu(), Placement::At { position }, window, cx, move |menu| {
            if !searchable {
                return menu;
            }
            menu.searchable("Search actions", |query, _| {
                let query = query.to_lowercase();
                sample_menu()
                    .into_iter()
                    .filter(|entry| match entry {
                        MenuEntry::Item(item) => item.label.to_lowercase().contains(&query),
                        _ => query.is_empty(),
                    })
                    .collect()
            })
        });
        let subscription = cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
            this.context_menu = None;
            cx.notify();
        });
        self.context_menu = Some((menu, subscription));
        cx.notify();
    }
}

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let t = theme.tokens.clone();
        let variants = [
            ("primary", ButtonVariant::Primary),
            ("inverse", ButtonVariant::Inverse),
            ("secondary", ButtonVariant::Secondary),
            ("soft", ButtonVariant::Soft),
            ("danger", ButtonVariant::Danger),
            ("ghost", ButtonVariant::Ghost),
            ("link", ButtonVariant::Link),
        ];

        let buttons = section("Buttons", cx)
            .child(row().children(variants.iter().map(|(name, variant)| {
                Button::new(SharedString::from(format!("b-{name}")), *name).variant(*variant).tip(format!("A {name} button (⌘B)"))
            })))
            .child(row().children(variants.iter().map(|(name, variant)| {
                Button::new(SharedString::from(format!("bi-{name}")), *name).icon(IconName::Plus).variant(*variant).size(ButtonSize::Lg)
            })))
            .child(row().children(variants.iter().map(|(name, variant)| {
                Button::new(SharedString::from(format!("bd-{name}")), *name).variant(*variant).disabled(true).size(ButtonSize::Sm)
            })))
            .child(
                row()
                    .child(Button::new("pill", "New thread").icon(IconName::SquarePen).pill(true))
                    .child(Button::new("busy", "Saving").busy(true))
                    .child(Button::new("armed", "Delete").variant(ButtonVariant::Danger).armed(true))
                    .child(Button::new("info", "Allow").variant(ButtonVariant::Primary).tone(FillTone::Info))
                    .child(Button::bare("square").icon(IconName::Settings).tip("Settings"))
                    .child(Button::new("toggle-on", "Pressed").variant(ButtonVariant::Ghost).pressed(true))
                    .child(Button::new("toggle-accent", "Accent").variant(ButtonVariant::Ghost).pressed(true).toggle_tone(ToggleTone::Accent))
                    .child(Button::new("reason", "Can't").disabled(true).reason("Not available on a remote computer"))
                    .child(ConfirmAction::new("confirm", "Delete project", "Delete", |_, _| {}).variant(ButtonVariant::Danger)),
            );

        let icon_buttons = section("Icon buttons", cx).child(
            row()
                .child(IconButton::new("ib-ghost", IconName::PanelLeft, "Toggle sidebar"))
                .child(IconButton::new("ib-soft", IconName::Copy, "Copy").variant(IconButtonVariant::Soft))
                .child(IconButton::new("ib-glass", IconName::Search, "Search").variant(IconButtonVariant::Glass).round(true))
                .child(IconButton::new("ib-inverse", IconName::ArrowUp, "Send").variant(IconButtonVariant::Inverse).round(true).size(IconButtonSize::Md))
                .child(IconButton::new("ib-inverse-off", IconName::ArrowUp, "Send").variant(IconButtonVariant::Inverse).round(true).size(IconButtonSize::Md).disabled(true))
                .child(IconButton::new("ib-danger", IconName::Trash2, "Delete").variant(IconButtonVariant::Danger))
                .child(IconButton::new("ib-pressed", IconName::ClipboardList, "Tasks").pressed(true))
                .child(IconButton::new("ib-xs", IconName::X, "Close (Esc)").size(IconButtonSize::Xs))
                .child(IconButton::new("ib-lg", IconName::X, "Close").size(IconButtonSize::Lg))
                .child(Spinning::new("spin", IconName::LoaderCircle).size(px(16.0)).color(t.info)),
        );

        let segmented = section("Segmented", cx)
            .child(
                row().child(
                    Segmented::new(
                        "seg",
                        vec![
                            SegmentedOption::new("dark", "Dark").icon(IconName::Moon),
                            SegmentedOption::new("light", "Light").icon(IconName::Sun),
                            SegmentedOption::new("auto", "Auto").icon(IconName::SunMoon),
                        ],
                        self.segment.clone(),
                    )
                    .on_change(cx.listener(|this, value: &SharedString, _, cx| {
                        this.segment = value.clone();
                        cx.notify();
                    })),
                ),
            )
            .child(
                Segmented::new(
                    "pills",
                    vec![
                        SegmentedOption::new("all", "All").count("12"),
                        SegmentedOption::new("failed", "Failed").count("2").tone(Tone::Danger),
                        SegmentedOption::new("running", "Running").count("3").tone(Tone::Info),
                    ],
                    self.pill.clone(),
                )
                .variant(SegmentedVariant::Pills)
                .on_change(cx.listener(|this, value: &SharedString, _, cx| {
                    this.pill = value.clone();
                    cx.notify();
                })),
            );

        let chips = section("Chips, keys, meters", cx)
            .child(
                row()
                    .child(Chip::count("4"))
                    .child(Chip::count("2").tone(Some(Tone::Danger)))
                    .child(Chip::count("9").tone(Some(Tone::Info)))
                    .child(Chip::tag("main"))
                    .child(Chip::tag("draft").tone(Some(Tone::Warning)))
                    .child(Chip::tag("merged").tone(Some(Tone::Success)))
                    .child(Chip::link("chip-link", "#42 Fix scroll"))
                    .child(Kbd::new(["⌘", "K"]))
                    .child(div().flex().items_center().text_size(px(12.0)).text_color(t.muted).child("Search").child(Kbd::inline(["⌘K"]))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .w(px(320.0))
                    .child(Meter::new(0.42).size(MeterSize::Hair))
                    .child(Meter::new(0.66).tone(Tone::Accent))
                    .child(Meter::new(0.9).size(MeterSize::Groove).tone(Tone::Danger)),
            );

        let notices = section("Notices", cx)
            .child(Notice::new("n1", "Threads a newer build wrote stay on disk.").icon(IconName::Info).tone(NoticeTone::Info))
            .child(
                Notice::new("n2", "The engine could not start.")
                    .icon(IconName::CircleAlert)
                    .tone(NoticeTone::Danger)
                    .action(Button::new("retry", "Retry").size(ButtonSize::Sm))
                    .on_dismiss(|_, _, _| {}),
            )
            .child(Notice::new("n3", "An update is ready.").icon(IconName::Download).tone(NoticeTone::Accent).floating(true).on_dismiss(|_, _, _| {}))
            .child(Notice::new("n4", "Rate limited until 3pm.").icon(IconName::TriangleAlert).tone(NoticeTone::Warning));

        let fields = section("Fields", cx)
            .child(div().w(px(320.0)).child(SearchField::new("search", &self.search).on_clear({
                let search = self.search.clone();
                move |_, _, cx| search.update(cx, |input, cx| input.set_text("", cx))
            })))
            .child(div().w(px(260.0)).child(SearchField::new("search-menu", &self.search).variant(SearchVariant::Menu)))
            .child(div().w(px(320.0)).child(TextField::new("name", &self.name).label("Project name").hint("Shown in the sidebar.")))
            .child(div().w(px(420.0)).child(TextField::new("composer", &self.composer)))
            .child(
                row()
                    .child(
                        Switch::new("switch", self.switch_on, "Notifications").on_change(cx.listener(|this, on: &bool, _, cx| {
                            this.switch_on = *on;
                            cx.notify();
                        })),
                    )
                    .child(
                        Checkbox::new("check", self.checked).label("Keep worktree").on_change(cx.listener(|this, on: &bool, _, cx| {
                            this.checked = *on;
                            cx.notify();
                        })),
                    )
                    .child(
                        Select::new(
                            "theme",
                            THEMES.iter().map(|info| SelectOption::new(info.id, info.label)).collect(),
                            self.select.clone(),
                        )
                        .on_change(cx.listener(|this, value: &SharedString, _, cx| {
                            this.select = value.clone();
                            Theme::change(cx, value, ThemeMode::Dark);
                            cx.notify();
                        })),
                    ),
            )
            .child(FieldGroup::new().label("Group").error(Some("That name is taken.".into())).child(Button::new("pick", "Choose folder")));

        let menus = section("Menus", cx).child(
            row()
                .child(PopoverMenu::new(
                    "pm-pill",
                    |_| MenuTrigger::new("trigger-pill").label("Sonnet").icon(IconName::Sparkles),
                    |_, _| sample_menu(),
                ))
                .child(PopoverMenu::new(
                    "pm-field",
                    |_| MenuTrigger::new("trigger-field").variant(TriggerVariant::Field).label("All projects").icon(IconName::Folder),
                    |_, _| sample_menu(),
                ))
                .child(
                    PopoverMenu::new(
                        "pm-search",
                        |_| MenuTrigger::new("trigger-ghost").variant(TriggerVariant::Ghost).label("Branch"),
                        |_, _| Vec::new(),
                    )
                    .configure(|menu| {
                        menu.searchable("Search branches", |query, _| {
                            ["main", "feature-auth", "fix-scroll", "release/0.10"]
                                .iter()
                                .filter(|branch| branch.contains(query))
                                .map(|branch| MenuItem::new(*branch).icon(IconName::GitBranch).into())
                                .collect()
                        })
                        .min_width(px(240.0))
                    }),
                )
                .child(Popover::new(
                    "usage",
                    |open| IconButton::new("usage-trigger", IconName::Gauge, "Usage").pressed(open).into_any_element(),
                    |_, cx| {
                        let t = &cx.theme().tokens;
                        div()
                            .w(px(240.0))
                            .p(px(12.0))
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .text_size(px(12.0))
                            .child(div().text_color(t.ink).font_weight(FontWeight::SEMIBOLD).child("Plan usage"))
                            .child(Meter::new(0.37).tone(Tone::Info))
                            .child(div().text_color(t.muted).child("37% of the 5-hour limit"))
                            .into_any_element()
                    },
                ))
                .child(MenuTrigger::new("trigger-chosen").label("Auto").icon(IconName::Zap).chosen(true))
                .child(MenuTrigger::new("trigger-danger").variant(TriggerVariant::Field).label("Full access").danger(true))
                .child(MenuTrigger::new("trigger-off").variant(TriggerVariant::Field).label("Read only").disabled(true))
                .child(
                    div()
                        .id("context-area")
                        .px(px(12.0))
                        .py(px(6.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_dashed()
                        .border_color(t.line_strong)
                        .text_size(px(12.0))
                        .text_color(t.muted)
                        .child("Right-click here")
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| this.open_context_menu(event.position, false, window, cx)),
                        ),
                ),
        );

        let lists = section("Lists", cx).child(
            div()
                .flex()
                .gap(px(16.0))
                .child(
                    nav_list()
                        .w(px(200.0))
                        .children(["General", "Appearance", "Engines"].iter().enumerate().map(|(index, label)| {
                            NavItem::new(("nav", index), *label, self.nav == index)
                                .icon([IconName::Settings, IconName::Palette, IconName::Cpu][index])
                                .count(if index == 2 { "3" } else { "" })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.nav = index;
                                    cx.notify();
                                }))
                        }))
                        .child(NavSeparator)
                        .child(NavItem::new("nav-archive", "Archive", false).icon(IconName::Archive)),
                )
                .child(
                    div()
                        .w(px(300.0))
                        .h(px(220.0))
                        .relative()
                        .rounded(px(10.0))
                        .bg(t.sunken)
                        .child(
                            div()
                                .id("list-scroll")
                                .size_full()
                                .overflow_y_scroll()
                                .track_scroll(&self.list_scroll)
                                .p(px(6.0))
                                .child(
                                    ListHeading::new("heading", "Threads")
                                        .count("12")
                                        .fold(!self.folded, cx.listener(|this, _, _, cx| {
                                            this.folded = !this.folded;
                                            cx.notify();
                                        }))
                                        .tip("Fold")
                                        .action(IconButton::new("heading-add", IconName::Plus, "New thread").size(IconButtonSize::Xs)),
                                )
                                .when(!self.folded, |this| {
                                    this.children((0..14usize).map(|index| {
                                        ListRow::new(("row", index), format!("Thread number {index}"))
                                            .detail("ai-coding-tool · Tue")
                                            .selected(index == 1)
                                            .leading(Icon::new(IconName::MessageSquare).size(px(14.0)))
                                            .trailing(Chip::count(format!("{index}")))
                                            .on_click(|_, _, _| {})
                                    }))
                                }),
                        )
                        .child(Scrollbar::vertical("list-scrollbar", &self.list_scroll)),
                ),
        );

        let mono = theme.typography.mono_family.clone();
        let type_specimens = section("Type", cx).children(
            [("System", ".SystemUIFont"), ("Inter", "Inter"), ("IBM Plex Sans", "IBM Plex Sans"), ("Geist", "Geist")]
                .iter()
                .map(|(label, family)| {
                    div()
                        .flex()
                        .gap(px(12.0))
                        .font_family(*family)
                        .text_size(px(14.0))
                        .child(div().w(px(110.0)).child(*label))
                        .child(div().font_weight(FontWeight::NORMAL).child("Regular"))
                        .child(div().font_weight(FontWeight(560.0)).child("Medium 560"))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("Semibold"))
                        .child(div().italic().child("Italic"))
                }),
        )
        .children(["JetBrains Mono", "Fira Code", "Source Code Pro"].iter().chain(std::iter::once(&mono.as_ref())).map(|family| {
            div().font_family(family.to_string()).text_size(px(13.0)).child(format!("{family}: fn main() -> Result<(), Error> {{ a != b => 0x1f }}"))
        }));

        let panels = section("Panels and surfaces", cx).child(
            div()
                .flex()
                .flex_col()
                .gap(px(16.0))
                .child(
                    div()
                        .w(px(380.0))
                        .h(px(260.0))
                        .flex()
                        .flex_col()
                        .rounded(px(11.0))
                        .bg(t.sunken)
                        .child(
                            PanelHeader::new("panel", "Diff")
                                .subtitle("3 files changed")
                                .icon(Icon::new(IconName::FileDiff).size(px(18.0)))
                                .action(IconButton::new("panel-refresh", IconName::RefreshCw, "Refresh"))
                                .on_close(|_, _, _| {}),
                        )
                        .child(toolbar(cx).child(IconButton::new("tb-back", IconName::ArrowLeft, "Back")).child(IconButton::new("tb-fwd", IconName::ArrowRight, "Forward")))
                        .child(EmptyState::new().icon(IconName::FileCheck2).title("No changes").detail("Edits the agent makes show up here.")),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(12.0))
                        .child(Surface::new(SurfaceKind::Float).p(px(14.0)).child("A float over the transcript"))
                        .child(Surface::new(SurfaceKind::Chip).px(px(10.0)).py(px(4.0)).child("A chip of glass"))
                        .child(Surface::new(SurfaceKind::Popover).tone(Tone::Danger).p(px(10.0)).child("A toned popover"))
                        .child(EmptyState::inline().title("Nothing yet.").detail("Start a thread."))
                        .child(Button::new("open-dialog", "Open dialog").on_click(cx.listener(|this, _, _, cx| {
                            this.dialog = true;
                            cx.notify();
                        })))
                        .child(Button::new("toast", "Show toast").on_click(cx.listener(|this, _, _, cx| {
                            let id = this.toasts.len() as u64 + 10;
                            this.toasts.push(ToastCard {
                                id,
                                tone: ToastTone::Success,
                                title: "Copied".into(),
                                message: Some("Paste it with `cmd v` anywhere.".into()),
                                action: None,
                            });
                            cx.notify();
                        }))),
                ),
        );

        let mut page = act_ui::root(cx)
            .id("gallery")
            .relative()
            .overflow_y_scroll()
            .p(px(28.0))
            .flex()
            .flex_col()
            .gap(px(22.0))
            .capture_any_mouse_down(|_, _, cx| act_ui::focus::note_pointer(cx))
            .child(div().text_size(px(21.0)).font_weight(FontWeight(600.0)).child(format!("act_ui gallery — {}", theme.info.label)))
            .child(
                div()
                    .flex()
                    .gap(px(36.0))
                    .child(div().flex().flex_col().gap(px(22.0)).flex_none().w(px(560.0)).child(buttons).child(icon_buttons).child(segmented).child(chips).child(notices))
                    .child(div().flex().flex_col().gap(px(22.0)).flex_none().w(px(520.0)).child(fields).child(menus).child(lists))
                    .child(div().flex().flex_col().gap(px(22.0)).flex_1().child(panels).child(type_specimens)),
            )
            .child(
                Toasts::new(self.toasts.clone()).on_dismiss(cx.listener(|this, id: &u64, _, cx| {
                    this.toasts.retain(|toast| toast.id != *id);
                    cx.notify();
                })),
            );
        if let Some((menu, _)) = &self.context_menu {
            page = page.child(menu.clone());
        }
        if self.dialog {
            let close = cx.listener(|this, _: &(), _, cx| {
                this.dialog = false;
                cx.notify();
            });
            let close = std::rc::Rc::new(close);
            let (a, b, c) = (close.clone(), close.clone(), close.clone());
            page = page.child(
                Dialog::new("dialog", move |window, cx| a(&(), window, cx))
                    .title("Delete this project?")
                    .child(dialog_paragraph("Its threads stay in the archive, and its folder on disk is left alone.", cx))
                    .action(Button::new("d-cancel", "Cancel").on_click(move |_, window, cx| b(&(), window, cx)))
                    .action(Button::new("d-ok", "Delete").variant(ButtonVariant::Danger).armed(true).on_click(move |_, window, cx| c(&(), window, cx))),
            );
        }
        page.child(tooltip_layer(window, cx))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let theme_id = args.iter().find(|arg| !arg.starts_with("--")).cloned();
    let open_dialog = args.iter().any(|arg| arg == "--dialog");
    let open_menu = args.iter().any(|arg| arg == "--menu");
    let open_search = args.iter().any(|arg| arg == "--search");
    let glass = args.iter().any(|arg| arg == "--glass");
    gpui_platform::application().with_assets(Assets).run(move |cx: &mut App| {
        act_ui::init(cx);
        if let Some(id) = &theme_id {
            Theme::change(cx, id, ThemeMode::Dark);
        }
        if glass {
            Theme::set_environment(cx, act_ui::theme::Environment::macos_glass());
        }
        println!("system mono: {}", act_ui::fonts::system_mono_family(cx));
        let bounds = Bounds::centered(None, size(px(1640.0), px(1000.0)), cx);
        let selected: SharedString = cx.theme().info.id.into();
        cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, |window, cx| {
            cx.new(|cx| {
                let search = cx.new(|cx| TextInput::new(cx).placeholder("Search threads"));
                let name = cx.new(|cx| TextInput::new(cx).with_text("ai-coding-tool"));
                let composer = cx.new(|cx| {
                    TextInput::new(cx)
                        .multiline(Autosize { min_rows: 2, max_height: Some(px(160.0)) })
                        .submit_on_enter(true)
                        .placeholder("Ask Claude anything")
                });
                let mut gallery = Gallery {
                    segment: "dark".into(),
                    pill: "all".into(),
                    nav: 0,
                    switch_on: true,
                    checked: true,
                    select: selected,
                    search,
                    name,
                    composer,
                    context_menu: None,
                    dialog: open_dialog,
                    toasts: vec![
                        ToastCard { id: 1, tone: ToastTone::Update, title: "Claude Code 2.1 is out".into(), message: Some("Run `npm i -g @anthropic-ai/claude-code` to update.".into()), action: Some("Update".into()) },
                        ToastCard { id: 2, tone: ToastTone::Progress, title: "Updating Codex".into(), message: None, action: None },
                    ],
                    list_scroll: ScrollHandle::new(),
                    folded: false,
                };
                if open_menu || open_search {
                    gallery.open_context_menu(gpui::point(px(720.0), px(420.0)), open_search, window, cx);
                }
                gallery
            })
        })
        .unwrap();
        cx.activate(true);
    });
}
