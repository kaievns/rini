use std::future::Future;
use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};
use objc2::MainThreadMarker;
use objc2_application_services::AXUIElement;
use rini::app::config::actor::{Change, ConfigActor};
use rini::app::config::watcher::ConfigWatcher;
use rini::app::hotkeys::{self as wm_controller, WmController};
use rini::app::launch_agent::{ServiceCommands, handle_service_command};
use rini::app::logging as log;
use rini::app::notifications::NotificationCenter;
use rini::app::reactor::{self, Reactor};
use rini::app::startup::execute_startup_commands;
use rini::displays::platform::mission_control::NativeMissionControl;
use rini::displays::platform::spaces::{Notification as SpacesNotification, SpacesActor};
use rini::displays::platform::window_notify as window_notify_actor;
use rini::displays::screen::displays_have_separate_spaces;
use rini::input::platform::gesture_tap::GestureTap;
use rini::input::platform::input_tap::InputTap;
use rini::windows::domain::transaction::WindowTxStore;
use rini::windows::platform::ax::permission::ensure_accessibility_permission;
use rini::windows::platform::lifecycle::ProcessActor;
use rini::windows::platform::sub_level::init_window_sub_level_server_port;
use rini::workspaces::LayoutEngine;
use rini::workspaces::domain::display_memory::DisplayMemory;
use rini_core::paths::restore_file;
use rini_geometry::space::{ScreenRect, ToCg};
use rini_ipc as ipc;
use rini_runloop::executor::Executor;
use rini_skylight_sys::{
    CGEnableEventStateCombining, CGSEventType, CGSetLocalEventsSuppressionInterval, KnownCGSEvent,
    SLSWindowManagementBridgeSetDelegate,
};
use tokio::join;

embed_plist::embed_info_plist!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/Info.plist"));

#[derive(Parser)]
struct Cli {
    /// Only run the window manager on the current space.
    #[arg(long)]
    one: bool,

    /// Start with every Space inactive, whatever activate-spaces-by-default says.
    ///
    /// Ignored if --one is used.
    #[arg(long)]
    default_disable: bool,

    /// Disable animations.
    #[arg(long)]
    no_animate: bool,

    /// Check the config file and the saved layout snapshot, then exit.
    #[arg(long)]
    validate: bool,

    /// Restore the saved layout on startup: window sizes, workspaces and strip
    /// positions from the last session. On by default.
    ///
    /// Accepted for compatibility and as an explicit override of --no-restore.
    #[arg(long)]
    restore: bool,

    /// Start from a clean layout, ignoring the saved one.
    #[arg(long)]
    no_restore: bool,

    /// Record reactor events to the specified file path. Overwrites the file if
    /// exists.
    #[arg(long)]
    record: Option<PathBuf>,

    /// Path to the config file (KDL) to use instead of ~/.config/rini/config.kdl.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage the launchd service for rini
    Service {
        #[command(subcommand)]
        service: ServiceCommands,
    },
}

/// this is okay because there is no recovery mechanism for actors
/// so we want to immediately exit (and most likely restart since
/// rini runs as a service most of the time)
///
/// Each poll is timed into the main thread's turn under `name`, for the journal's `main_turn`.
async fn supervise(name: &'static str, fut: impl Future<Output = ()>) {
    rini_runloop::turns::timed(name, fut).await;
    panic!("{name} exited");
}

/// Everything that has to be true of the process before rini does anything.
///
/// Backtraces on by default, because the one crash report that matters is the one from a user who did
/// not know to set the variable. Accessory activation policy so rini has no Dock icon and no menu bar
/// of its own. `finishLaunching` before any window is touched, or AppKit refuses the first one.
///
/// `SLSWindowManagementBridgeSetDelegate(null)` detaches the window server's own management bridge.
/// Without it macOS applies its own tiling to windows rini is moving and the two fight.
fn prepare_process() -> MainThreadMarker {
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        // SAFETY: We are single threaded at this point.
        unsafe { std::env::set_var("RUST_BACKTRACE", "1") };
    }
    log::init_logging();
    install_panic_hook();

    let mtm = MainThreadMarker::new().unwrap();
    {
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
        let app = NSApplication::sharedApplication(mtm);
        let _ = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
        NSApplication::load();
    }

    unsafe { SLSWindowManagementBridgeSetDelegate(std::ptr::null_mut()) };
    mtm
}

/// The three things rini cannot run without. Each either blocks until granted or exits saying why.
///
/// "Displays have separate Spaces" is a hard requirement rather than a degraded mode: with it off,
/// macOS gives every display one shared space, so a per-display strip has nowhere to live and every
/// space query answers about the wrong screen.
fn preflight() {
    ensure_accessibility_permission();
    init_window_sub_level_server_port();

    if !displays_have_separate_spaces() {
        eprintln!(
            "Rini detected that the macOS setting \"Displays have separate Spaces\" \
is disabled. Rini currently requires this setting to be enabled. \
Enable it in System Settings > Desktop & Dock (Mission Control) and restart Rini."
        );
        std::process::exit(1);
    }
}

fn main() {
    sigpipe::reset();
    let opt = Cli::parse();

    if let Some(Commands::Service { service }) = &opt.command {
        match handle_service_command(service) {
            Ok(msg) => {
                println!("{}", msg);
                process::exit(0);
            }
            Err(e) => {
                eprintln!("{}", e);
                process::exit(1);
            }
        }
    }

    let mtm = prepare_process();
    preflight();

    let config_path = rini::app::boot::config_path(opt.config.clone());
    if opt.validate {
        match rini::app::boot::validate_config(&config_path) {
            Ok(line) => println!("{line}"),
            Err(report) => {
                eprintln!("{report}");
                process::exit(1);
            }
        }
        let path = restore_file();
        match LayoutEngine::load(path.clone()) {
            Ok(_) => println!("Saved layout file is valid: {}", path.display()),
            Err(error) => {
                eprintln!(
                    "Could not load the saved layout file at {}: {error}",
                    path.display()
                );
                process::exit(1);
            }
        }
        process::exit(0);
    }

    // The fallback rule is `app::boot::config_or_default`, which is also where its tests are: this
    // path is only reachable by running the binary.
    let (mut config, complaint) = rini::app::boot::config_or_default(&config_path);
    let config_usable = complaint.is_none();
    if let Some(complaint) = complaint {
        eprintln!("{complaint}");
    }
    rini::app::boot::apply_flag_overrides(
        &mut config.settings,
        opt.no_animate,
        opt.default_disable,
    );

    execute_startup_commands(&config.settings.run_on_start);

    let (broadcast_tx, broadcast_rx) = rini::app::channels::channel();

    // Restore is ON by default, so a restart or redeploy keeps window sizes, workspaces
    // and strip positions.
    //
    // It was opt-in for a while because enabling it exposed two latent bugs in a code
    // path nothing had exercised. Restored workspaces kept a stale layout MODE, and
    // windows belonging to a display that had since been unplugged were restored at
    // that display's coordinates — measured at x=-1680 with no display there — and
    // stranded, with nothing to migrate them back. A single dock/undock cycle then
    // produced a layout only fixable by deleting ~/.rini/layout.ron.
    //
    // Both are handled now. The layout mode is migrated on restore, and
    // release_windows_saved_on_absent_displays gives up the saved slots of any display
    // that is not attached while keeping each window's display affinity, so those
    // windows lay out fresh on a live display and return to their own display when it
    // is plugged back in.
    let want_restore = rini::app::boot::wants_restore(opt.restore, opt.no_restore);
    let fresh = || LayoutEngine::new(&config.virtual_workspaces, &config.settings.layout);
    let (mut layout, display_memory) = if want_restore {
        let path = restore_file();
        match LayoutEngine::load_for_startup_restore(path.clone()) {
            Ok(loaded) => loaded,
            Err(error) => {
                eprintln!(
                    "Could not restore the saved layout file at {}; starting with a fresh \
                     layout: {error}",
                    path.display()
                );
                (fresh(), DisplayMemory::default())
            }
        }
    } else {
        (fresh(), DisplayMemory::default())
    };
    layout.finish_loading(&config.virtual_workspaces, &config.settings.layout);
    let (event_tap_tx, event_tap_rx) = rini::app::channels::channel();
    let (wnd_tx, wnd_rx) = rini::app::channels::channel();
    let window_tx_store = WindowTxStore::new();
    let (gesture_tap_tx, gesture_tap_rx) = rini::app::channels::channel();
    let (cursor_warp_tx, cursor_warp_rx) = rini::app::channels::channel();
    let (transitions_tx, transitions_rx) = rini::app::channels::channel();
    let (switcher_tx, switcher_rx) = rini::app::channels::channel();
    let (bar_tx, bar_rx) = rini::app::channels::channel();
    let (rest_tx, rest_rx) = rini::app::channels::channel();
    // Whether the window server's frames and orders are followed for the rest windows: the reactor
    // turns it on while any window can wear a band.
    let following = rini::displays::event::Following::default();
    let (spaces_tx, spaces_rx) = rini::app::channels::channel();

    // Every background picture rini draws, taken on the service's own thread. The app wires its sinks,
    // so the service names neither the flight engine nor the switcher.
    let pictures = rini::pictures::platform::service::Pictures::start(vec![
        Box::new({
            let transitions_tx = transitions_tx.clone();
            move |landed| {
                transitions_tx.send(rini::transitions::platform::engine::Event::Pictures(landed))
            }
        }),
        Box::new({
            let switcher_tx = switcher_tx.clone();
            move |landed| {
                if let rini::pictures::event::Event::Taken { window, .. } = landed {
                    switcher_tx.send(rini::switcher::platform::actor::Event::Landed(window));
                }
            }
        }),
    ]);

    let reactor = Reactor::spawn(
        config.clone(),
        layout,
        display_memory,
        reactor::Record::new(opt.record.as_deref()),
        event_tap_tx.clone(),
        broadcast_tx.clone(),
        Some(cursor_warp_tx.clone()),
        Some(transitions_tx.clone()),
        Some(pictures.clone()),
        Some(switcher_tx.clone()),
        Some(bar_tx.clone()),
        Some((rest_tx.clone(), following.clone())),
        Some((wnd_tx.clone(), window_tx_store.clone())),
        Some(gesture_tap_tx.clone()),
        opt.one,
    );
    let events_tx = reactor.sender();
    if !config_usable {
        events_tx.send(reactor::Event::ConfigFileRead { usable: false });
    }

    // The band the bar keeps clear is the spaces actor's to apply, so it hears of every accepted
    // change directly rather than through the reactor.
    let config_tx = ConfigActor::spawn_with_path(
        config.clone(),
        Box::new({
            let events_tx = events_tx.clone();
            let spaces_tx = spaces_tx.clone();
            move |change| match change {
                Change::Applied(config) => {
                    spaces_tx.send(SpacesNotification::TopBandChanged(config.settings.top_band()));
                    events_tx.send(reactor::Event::ConfigUpdated(config))
                }
                Change::FileRead { usable } => {
                    events_tx.send(reactor::Event::ConfigFileRead { usable })
                }
            }
        }),
        config_path.clone(),
    );

    ConfigWatcher::spawn(config_tx.clone(), config_path.clone());

    let server_state = match ipc::run_mach_server(rini::app::api::backend::IpcBackend::new(
        reactor.clone(),
        config_tx.clone(),
    )) {
        Ok(state) => state,
        Err(err) => {
            eprintln!("{}", err);
            process::exit(1);
        }
    };

    let mach_bridge_rx = broadcast_rx;

    let server_state_for_bridge = server_state.clone();
    std::thread::spawn(move || {
        let mut rx = mach_bridge_rx;
        let server_state = server_state_for_bridge;
        loop {
            match rx.blocking_recv() {
                Some((_span, event)) => {
                    let state = server_state.read();
                    state.publish(event);
                }
                None => {
                    break;
                }
            }
        }
    });

    let wm_config = wm_controller::Config {
        restore_file: restore_file(),
        config: config.clone(),
    };
    let (wm_controller, wm_controller_sender) = WmController::new(
        wm_config,
        config_tx.clone(),
        events_tx.clone(),
        event_tap_tx.clone(),
        Some(gesture_tap_tx.clone()),
        Some(window_tx_store.clone()),
    );

    let _ = events_tx.send(reactor::Event::RegisterWmSender(wm_controller_sender.clone()));

    let spaces_actor = SpacesActor::new(
        spaces_rx,
        spaces_tx.clone(),
        Box::new(wm_controller_sender.clone()),
        config.settings.top_band(),
    );
    let wn_actor = window_notify_actor::WindowNotify::new(
        events_tx.clone(),
        spaces_tx.clone(),
        wnd_rx,
        &[
            // this event seems bugged
            //CGSEventType::Known(KnownCGSEvent::SpaceCurrentChanged),
            // repl for aboce
            CGSEventType::Known(KnownCGSEvent::WorkspaceDidChange),
            CGSEventType::Known(KnownCGSEvent::ManagedSpaceMembershipUpdated),
            CGSEventType::Known(KnownCGSEvent::SpaceWindowManagementCapabilitiesChanged),
            CGSEventType::Known(KnownCGSEvent::SpaceWindowDestroyed),
            CGSEventType::Known(KnownCGSEvent::SpaceWindowCreated),
            // Native focus wakeups. Payload identities are racy, so adjacent
            // events are coalesced before re-querying WindowServer key focus.
            CGSEventType::Known(KnownCGSEvent::WindowReordered),
            CGSEventType::Known(KnownCGSEvent::WindowUnhidden),
            CGSEventType::Known(KnownCGSEvent::WindowHidden),
            CGSEventType::Known(KnownCGSEvent::WindowClosed),
            CGSEventType::Known(KnownCGSEvent::WindowManagerSpaceFrontConnectionChanged),
            CGSEventType::Known(KnownCGSEvent::WindowManagerGlobalFrontConnectionChanged),
            CGSEventType::Known(KnownCGSEvent::SpaceCreated),
            CGSEventType::Known(KnownCGSEvent::SpaceDestroyed),
            CGSEventType::Known(KnownCGSEvent::WindowMoved),
            CGSEventType::Known(KnownCGSEvent::WindowResized),
        ],
        // The rest windows follow every decorated window the window server moves, go back above the
        // ones it reorders, and hide with the ones it hides or closes, while `following` is on.
        vec![Box::new({
            let rest_tx = rest_tx.clone();
            move |frames: &[_]| rest_tx.send(rini::decorations::platform::rest::Event::Frames(frames.to_vec()))
        })],
        vec![Box::new({
            let rest_tx = rest_tx.clone();
            move |order| rest_tx.send(rini::decorations::platform::rest::Event::Order(order))
        })],
        following,
    );

    let notification_center = NotificationCenter::new(wm_controller_sender.clone(), spaces_tx);

    // Which applications ask for attention, read from LaunchServices on a thread of its own.
    rini::windows::platform::attention::spawn(Box::new(events_tx.clone()));

    let process_actor = ProcessActor::new({
        let sender = wm_controller_sender.clone();
        move |event| sender.send(event.into())
    });

    let input_settings = rini::input::settings::InputSettings::from(&config);
    let mut event_tap = InputTap::new(
        &input_settings,
        rini::app::power::is_low_power_mode_enabled(),
        Box::new(wm_controller_sender.clone()),
        event_tap_rx,
    );
    event_tap.set_on_left_press(reactor::Event::left_press_sink(events_tx.clone()));
    let gesture_tap = GestureTap::new(
        input_settings,
        Box::new(wm_controller_sender.clone()),
        gesture_tap_rx,
    );

    // Warping needs no main-thread access and no permissions, so it is just another
    // actor. It stays parked until the reactor sends it geometry for two or more displays.
    let cursor_warp = rini::displays::platform::cursor_warp::CursorWarp::new(
        config.settings.warp_cursor_between_stacked_displays,
        config.settings.stacked_display_upper_is,
        config.settings.stacked_display_lower_top_at,
        cursor_warp_rx,
    );

    // The renderer owns the overlay, on the main thread because Core Animation requires it, and plays
    // what the flight engine draws. The engine has a thread of its own, and is made there: its timers
    // run on the loop that makes them. Both stay idle until the reactor sends display geometry and
    // something to animate.
    // The decorations drawn at rest, on the main thread for AppKit: a rest window above each window
    // whose band shows, told what each wears and when each flight begins by the reactor. From a
    // flight's begin to its lift they draw nothing, and the renderer places them before each lift
    // hides the overlay, so the lift shows each band where its tile had it.
    let (rest_actor, rest_handover) =
        rini::decorations::platform::rest::RestActor::new(rest_rx, rest_tx.clone(), mtm);
    let (render_tx, render_rx) = rini::app::channels::channel();
    let renderer = rini::animation::platform::renderer::Renderer::new(
        render_rx,
        Box::new({
            let transitions_tx = transitions_tx.clone();
            move |answer| {
                transitions_tx.send(rini::transitions::platform::engine::Event::Rendered(answer))
            }
        }),
        rest_handover,
        mtm,
    );
    let flight_engine = rini::transitions::platform::engine::Parts {
        rx: transitions_rx,
        tx: transitions_tx.clone(),
        pictures: pictures.clone(),
        client: rini::pictures::domain::watch::Client(1),
        draw: Box::new(move |batch| render_tx.send(batch)),
        place_frames: Box::new({
            let events_tx = events_tx.clone();
            move |frames: Vec<(_, ScreenRect)>| {
                let frames = frames.into_iter().map(|(window, frame)| (window, frame.to_cg()));
                events_tx.send(reactor::Event::ApplyOverlayFrames(frames.collect()))
            }
        }),
        // The bar stops picturing the menu extras while a flight runs and settles.
        on_flight: Box::new({
            let bar_tx = bar_tx.clone();
            move |flying| bar_tx.send(rini::bar::platform::actor::Event::Flight(flying))
        }),
        media: rini::animation::platform::renderer::media_clock(),
        clock: Box::new(std::time::Instant::now),
    };
    std::thread::Builder::new()
        .name("transitions".into())
        .spawn(move || {
            let engine = rini::transitions::platform::engine::FlightEngine::new(flight_engine);
            Executor::run_interactive(engine.run());
            panic!("transitions thread exited");
        })
        .expect("failed to spawn transitions thread");

    // The switcher popup, on the main thread for the same reason the overlay is: AppKit and Core
    // Animation require it. Idle until a switch opens, and it builds its window on first use.
    // A click on a popup row goes to the reactor, which owns the selection; wired here because the
    // switcher may not name the app layer.
    let switcher_actor = rini::switcher::platform::actor::SwitcherActor::new(
        switcher_rx,
        mtm,
        std::rc::Rc::new({
            let events_tx = events_tx.clone();
            move |window| events_tx.send(reactor::Event::SwitchPicked(window))
        }),
        pictures,
    );

    // Every display's bar, on the main thread for AppKit. It is sent what it shows by the reactor, and
    // a click on it goes back there; wired here because the bar may not name the app layer.
    let bar_actor = rini::bar::platform::actor::BarActor::new(
        bar_rx,
        bar_tx,
        mtm,
        std::rc::Rc::new({
            let events_tx = events_tx.clone();
            move |action| events_tx.send(reactor::Event::BarAction(action))
        }),
    );

    let mission_control_native = NativeMissionControl::new(events_tx.clone());

    if config.settings.default_disable {
        println!(
            "NOTICE: rini starts with every Space inactive. Turn a Space on with the \
             toggle-space-activated action (Mod+Z in rini.default.kdl), or write \
             activate-spaces-by-default in the config to manage every Space from the start."
        );
    }

    unsafe { AXUIElement::new_system_wide().set_messaging_timeout(1.0) };

    CGSetLocalEventsSuppressionInterval(0.0);
    CGEnableEventStateCombining(false);

    // The event tap runs on a dedicated thread with its own CFRunLoop,
    // isolated from main-thread stalls (layout, animation, SLS IPC).
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            rini_runloop::executor::Executor::run(event_tap.run());
            panic!("input thread exited");
        })
        .expect("failed to spawn input thread");

    // Every main-thread turn over a frame goes to the flight engine's journal.
    let _turns = rini_runloop::run_loop::TurnObserver::install({
        let transitions_tx = transitions_tx.clone();
        move |turn| transitions_tx.send(rini::transitions::platform::engine::Event::MainTurn(turn))
    });

    Executor::run_main(mtm, async move {
        join!(
            supervise("wm_controller", wm_controller.run()),
            supervise(
                "notification_center",
                notification_center.watch_for_notifications()
            ),
            supervise("spaces", spaces_actor.run()),
            supervise("gesture_tap", gesture_tap.run()),
            supervise("window_notify", wn_actor.run()),
            supervise("mc_native", mission_control_native.run()),
            supervise("process_actor", process_actor.run()),
            supervise("cursor_warp", cursor_warp.run()),
            supervise("renderer", renderer.run()),
            supervise("switcher", switcher_actor.run()),
            supervise("bar", bar_actor.run()),
            supervise("rest", rest_actor.run()),
        );
    });
}

#[cfg(panic = "unwind")]
fn install_panic_hook() {
    // Abort on panic instead of propagating panics to the main thread.
    // See Cargo.toml for why we don't use panic=abort everywhere.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        original_hook(info);
        std::process::abort();
    }));
}

#[cfg(not(panic = "unwind"))]
fn install_panic_hook() {}
