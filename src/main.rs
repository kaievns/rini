use std::future::Future;
use std::process;

use clap::Parser;
use objc2::MainThreadMarker;
use objc2_application_services::AXUIElement;
use rini::app::boot::{Cli, Commands};
use rini::app::config::Config;
use rini::app::config::actor::{Change, ConfigActor};
use rini::app::config::watcher::ConfigWatcher;
use rini::app::controller::WmController;
use rini::app::launch_agent::handle_service_command;
use rini::app::notifications::NotificationCenter;
use rini::app::reactor::{self, Reactor, ReactorParts};
use rini::app::startup::execute_startup_commands;
use rini::app::{channels, logging as log};
use rini::displays::platform::mission_control::NativeMissionControl;
use rini::displays::platform::spaces::{Notification as SpacesNotification, SpacesActor};
use rini::displays::platform::window_notify as window_notify_actor;
use rini::displays::screen::displays_have_separate_spaces;
use rini::input::platform::input_tap::InputTap;
use rini::windows::domain::transaction::WindowTxStore;
use rini::windows::platform::ax::permission::ensure_accessibility_permission;
use rini::windows::platform::lifecycle::ProcessActor;
use rini::windows::platform::sub_level::init_window_sub_level_server_port;
use rini::workspaces::LayoutEngine;
use rini::workspaces::domain::display_memory::DisplayMemory;
use rini_core::ids::{WindowId, WindowServerId};
use rini_core::paths::restore_file;
use rini_geometry::space::{ScreenRect, ToCg};
use rini_ipc as ipc;
use rini_runloop::executor::Executor;
use rini_skylight_sys::{
    CGEnableEventStateCombining, CGSetLocalEventsSuppressionInterval, KnownCGSEvent,
    SLSWindowManagementBridgeSetDelegate,
};
use tokio::join;

embed_plist::embed_info_plist!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/Info.plist"));

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
    // Opened now, after the main connection, rather than by the first read on whichever thread.
    once_cell::sync::Lazy::force(&rini::windows::platform::window_server::READ_CONNECTION);

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
    let (mtm, config_path) = start(&opt);
    let (config, config_usable) = read_config(&config_path, opt.no_animate);

    execute_startup_commands(&config.settings.run_on_start);

    let (mut layout, display_memory) = restored_layout(&config, opt.no_restore);
    layout.finish_loading(&config.virtual_workspaces, &config.settings.layout);
    let (event_tap_tx, event_tap_rx) = rini::app::channels::channel();
    let window_tx_store = WindowTxStore::new();
    let (cursor_warp_tx, cursor_warp_rx) = rini::app::channels::channel();
    let (transitions_tx, transitions_rx) = rini::app::channels::channel();
    let (switcher_tx, switcher_rx) = rini::app::channels::channel();
    let (bar_tx, bar_rx) = rini::app::channels::channel();
    let (rest_tx, rest_rx) = rini::app::channels::channel();
    // Whether the window server's frames and orders are followed for the rest windows, and the
    // frames thread's queue they and the window notifications ask it through: the reactor turns it
    // on while any window can wear a band.
    let (following, server_reads) = rini::displays::event::Following::new();
    let (spaces_tx, spaces_rx) = rini::app::channels::channel();
    let (churn_writer, display_churn) = rini::displays::platform::display_churn::shared();

    let pictures = start_pictures(&transitions_tx, &switcher_tx);

    let reactor = Reactor::spawn(ReactorParts {
        config: config.clone(),
        layout_engine: layout,
        display_memory,
        event_tap_tx: event_tap_tx.clone(),
        cursor_warp_tx: Some(cursor_warp_tx.clone()),
        transitions_tx: Some(transitions_tx.clone()),
        pictures: Some(pictures.clone()),
        switcher_tx: Some(switcher_tx.clone()),
        bar_tx: Some(bar_tx.clone()),
        rest: Some((rest_tx.clone(), following.clone())),
        window_tx_store: window_tx_store.clone(),
        display_churn,
    });
    let events_tx = reactor.sender();
    if !config_usable {
        events_tx.send(reactor::Event::ConfigFileRead { usable: false });
    }

    let config_tx = spawn_config_actor(&config, &events_tx, &spaces_tx, &config_path);
    ConfigWatcher::spawn(config_tx.clone(), config_path.clone());
    serve_ipc(&reactor, &config_tx);

    let (wm_controller, wm_controller_sender) = WmController::new(
        config.clone(),
        config_tx.clone(),
        events_tx.clone(),
        event_tap_tx.clone(),
        Some(window_tx_store.clone()),
    );
    events_tx.send(reactor::Event::RegisterWmSender(wm_controller_sender.clone()));

    let spaces = SpacesActor::new(
        spaces_rx,
        spaces_tx.clone(),
        Box::new(events_tx.clone()),
        churn_writer,
        config.settings.top_band(),
    );
    let window_notify =
        window_notify(&events_tx, &spaces_tx, &rest_tx, following.clone(), server_reads);
    let notification_center = NotificationCenter::new(wm_controller_sender.clone(), spaces_tx);

    // Which applications ask for attention, read from LaunchServices on a thread of its own.
    rini::windows::platform::attention::spawn(Box::new(events_tx.clone()));

    let process = process_actor(&wm_controller_sender);
    let event_tap = input_tap(&config, &wm_controller_sender, event_tap_rx, &events_tx);
    let cursor_warp = cursor_warp(&config, cursor_warp_rx);

    // The decorations drawn at rest, on the main thread for AppKit: a rest window above each window
    // whose band shows, told what each wears and when each flight begins by the reactor. From a
    // flight's begin they draw nothing until the renderer places them, a tick before the lift once
    // the flight has landed or in the lift's drain, so the lift shows each band where its tile had it.
    let (rest, rest_handover) =
        rini::decorations::platform::rest::RestActor::new(rest_rx, rest_tx.clone(), following, mtm);
    let (render_tx, render_rx) = rini::app::channels::channel();
    let renderer = renderer(render_rx, &transitions_tx, rest_handover, mtm);
    spawn_transitions(flight_engine_parts(
        transitions_rx,
        &transitions_tx,
        pictures.clone(),
        render_tx,
        &events_tx,
        &bar_tx,
    ));
    let switcher = switcher_actor(switcher_rx, mtm, &events_tx, pictures);
    let bar = bar_actor(bar_rx, bar_tx, mtm, &events_tx);
    let mission_control = NativeMissionControl::new(events_tx.clone());

    tune_the_system(&config);
    spawn_input(event_tap);

    // Every main-thread turn over a frame goes to the flight engine's journal.
    let _turns = rini_runloop::run_loop::TurnObserver::install({
        let transitions_tx = transitions_tx.clone();
        move |turn| transitions_tx.send(rini::transitions::platform::engine::Event::MainTurn(turn))
    });

    run_main_thread(
        mtm,
        QuietActors {
            wm_controller,
            notification_center,
            spaces,
            window_notify,
            mission_control,
            process,
            cursor_warp,
        },
        DrawingActors { renderer, switcher, bar, rest },
    );
}

/// The actors on the main thread that draw nothing.
struct QuietActors {
    wm_controller: WmController,
    notification_center: NotificationCenter,
    spaces: SpacesActor,
    window_notify: window_notify_actor::WindowNotify,
    mission_control: NativeMissionControl,
    process: ProcessActor,
    cursor_warp: rini::displays::platform::cursor_warp::CursorWarp,
}

/// The actors on the main thread because AppKit and Core Animation require it.
struct DrawingActors {
    renderer: rini::animation::platform::renderer::Renderer,
    switcher: rini::switcher::platform::actor::SwitcherActor,
    bar: rini::bar::platform::actor::BarActor,
    rest: rini::decorations::platform::rest::RestActor,
}

/// Every main-thread actor joined on the main thread's executor, each supervised.
fn run_main_thread(mtm: MainThreadMarker, quiet: QuietActors, drawing: DrawingActors) {
    Executor::run_main(mtm, async move {
        join!(
            supervise("wm_controller", quiet.wm_controller.run()),
            supervise(
                "notification_center",
                quiet.notification_center.watch_for_notifications()
            ),
            supervise("spaces", quiet.spaces.run()),
            supervise("window_notify", quiet.window_notify.run()),
            supervise("mc_native", quiet.mission_control.run()),
            supervise("process_actor", quiet.process.run()),
            supervise("cursor_warp", quiet.cursor_warp.run()),
            supervise("renderer", drawing.renderer.run()),
            supervise("switcher", drawing.switcher.run()),
            supervise("bar", drawing.bar.run()),
            supervise("rest", drawing.rest.run()),
        );
    });
}

/// The process prepared and its prerequisites checked, after a `service` subcommand or
/// `--validate` has run and exited; the config file's path.
fn start(opt: &Cli) -> (MainThreadMarker, std::path::PathBuf) {
    if let Some(Commands::Service { service }) = &opt.command {
        run_service_command(service);
    }

    let mtm = prepare_process();
    preflight();

    let config_path = rini::app::boot::config_path(opt.config.clone());
    if opt.validate {
        validate_and_exit(&config_path);
    }
    (mtm, config_path)
}

/// The config, or the defaults when the file cannot be used, said so on stderr; and whether the
/// file could be used.
fn read_config(config_path: &std::path::Path, no_animate: bool) -> (Config, bool) {
    // The fallback rule is `app::boot::config_or_default`, which is also where its tests are: this
    // path is only reachable by running the binary.
    let (mut config, complaint) = rini::app::boot::config_or_default(config_path);
    let config_usable = complaint.is_none();
    if let Some(complaint) = complaint {
        eprintln!("{complaint}");
    }
    rini::app::boot::apply_no_animate(&mut config.settings, no_animate);
    (config, config_usable)
}

/// The IPC server the CLI talks to, or the process exited saying why it could not start.
fn serve_ipc(reactor: &reactor::ReactorHandle, config_tx: &rini::app::config::actor::Sender) {
    if let Err(err) = ipc::run_mach_server(rini::app::api::backend::IpcBackend::new(
        reactor.clone(),
        config_tx.clone(),
    )) {
        eprintln!("{}", err);
        process::exit(1);
    }
}

fn process_actor(wm_controller_sender: &rini::app::controller::Sender) -> ProcessActor {
    ProcessActor::new({
        let sender = wm_controller_sender.clone();
        move |event| sender.send(event.into())
    })
}

/// The event tap, told of the input settings and Low Power Mode, and handing each left press to
/// the reactor.
fn input_tap(
    config: &Config,
    wm_controller_sender: &rini::app::controller::Sender,
    event_tap_rx: channels::Receiver<rini::input::platform::input_tap::Request>,
    events_tx: &reactor::Sender,
) -> InputTap {
    let input_settings = rini::input::settings::InputSettings::from(config);
    let mut event_tap = InputTap::new(
        &input_settings,
        rini::app::power::is_low_power_mode_enabled(),
        Box::new(wm_controller_sender.clone()),
        event_tap_rx,
    );
    event_tap.set_on_left_press(reactor::Event::left_press_sink(events_tx.clone()));
    event_tap
}

/// Warping needs no main-thread access and no permissions, so it is just another
/// actor. It stays parked until the reactor sends it geometry for two or more displays.
fn cursor_warp(
    config: &Config,
    cursor_warp_rx: channels::Receiver<rini::displays::platform::cursor_warp::Request>,
) -> rini::displays::platform::cursor_warp::CursorWarp {
    rini::displays::platform::cursor_warp::CursorWarp::new(
        config.settings.warp_cursor_between_stacked_displays,
        config.settings.stacked_display_upper_is,
        config.settings.stacked_display_lower_top_at,
        cursor_warp_rx,
    )
}

/// The renderer owns the overlay, on the main thread because Core Animation requires it, and plays
/// what the flight engine draws. The engine has a thread of its own, and is made there: its timers
/// run on the loop that makes them. Both stay idle until the reactor sends display geometry and
/// something to animate.
fn renderer(
    render_rx: channels::Receiver<rini::animation::platform::renderer::Batch>,
    transitions_tx: &channels::Sender<rini::transitions::platform::engine::Event>,
    rest_handover: rini::decorations::platform::rest::Handover,
    mtm: MainThreadMarker,
) -> rini::animation::platform::renderer::Renderer {
    rini::animation::platform::renderer::Renderer::new(
        render_rx,
        Box::new({
            let transitions_tx = transitions_tx.clone();
            move |answer| {
                transitions_tx.send(rini::transitions::platform::engine::Event::Rendered(answer))
            }
        }),
        rest_handover,
        mtm,
    )
}

/// The switcher popup, on the main thread for the same reason the overlay is: AppKit and Core
/// Animation require it. Idle until a switch opens, and it builds its window on first use.
/// A click on a popup row goes to the reactor, which owns the selection; wired here because the
/// switcher may not name the app layer.
fn switcher_actor(
    switcher_rx: channels::Receiver<rini::switcher::platform::actor::Event>,
    mtm: MainThreadMarker,
    events_tx: &reactor::Sender,
    pictures: rini::pictures::platform::service::Pictures,
) -> rini::switcher::platform::actor::SwitcherActor {
    rini::switcher::platform::actor::SwitcherActor::new(
        switcher_rx,
        mtm,
        std::rc::Rc::new({
            let events_tx = events_tx.clone();
            move |window| events_tx.send(reactor::Event::SwitchPicked(window))
        }),
        pictures,
    )
}

/// Every display's bar, on the main thread for AppKit. It is sent what it shows by the reactor, and
/// a click on it goes back there; wired here because the bar may not name the app layer.
fn bar_actor(
    bar_rx: channels::Receiver<rini::bar::platform::actor::Event>,
    bar_tx: channels::Sender<rini::bar::platform::actor::Event>,
    mtm: MainThreadMarker,
    events_tx: &reactor::Sender,
) -> rini::bar::platform::actor::BarActor {
    rini::bar::platform::actor::BarActor::new(
        bar_rx,
        bar_tx,
        mtm,
        std::rc::Rc::new({
            let events_tx = events_tx.clone();
            move |action| events_tx.send(reactor::Event::BarAction(action))
        }),
    )
}

/// The notice that every Space starts inactive, when it does, and the system settings rini runs
/// under: a one-second Accessibility messaging timeout, no local events suppression after a warp,
/// and no event state combining.
fn tune_the_system(config: &Config) {
    if !config.settings.activate_spaces_by_default {
        println!(
            "NOTICE: rini starts with every Space inactive. Turn a Space on with the \
             toggle-space-activated action (Mod+Z in rini.default.kdl), or write \
             activate-spaces-by-default in the config to manage every Space from the start."
        );
    }

    unsafe { AXUIElement::new_system_wide().set_messaging_timeout(1.0) };

    CGSetLocalEventsSuppressionInterval(0.0);
    CGEnableEventStateCombining(false);
}

/// Carries out a `service` subcommand and exits with its result.
fn run_service_command(service: &rini::app::launch_agent::ServiceCommands) -> ! {
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

/// `--validate`: the config file and the saved layout checked, each reported, and the process
/// exited with whether both are valid.
fn validate_and_exit(config_path: &std::path::Path) -> ! {
    match rini::app::boot::validate_config(config_path) {
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

/// The layout to start from and the display memory it was saved with: the saved one unless
/// `no_restore`, and a fresh one when there is none or it cannot be read.
///
/// Restore is on by default, so a restart or redeploy keeps window sizes, workspaces and strip
/// positions. A display that is not attached has its saved slots released, its windows keeping
/// their display affinity (`release_windows_saved_on_absent_displays`,
/// `src/workspaces/docs/workspaces-and-displays.md`).
fn restored_layout(config: &Config, no_restore: bool) -> (LayoutEngine, DisplayMemory) {
    let fresh = || LayoutEngine::new(&config.virtual_workspaces, &config.settings.layout);
    if !no_restore {
        let path = restore_file();
        match load_saved_layout(path.clone()) {
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
    }
}

/// Every background picture rini draws, taken on the service's own thread. The app wires its sinks,
/// so the service names neither the flight engine nor the switcher.
fn start_pictures(
    transitions_tx: &channels::Sender<rini::transitions::platform::engine::Event>,
    switcher_tx: &channels::Sender<rini::switcher::platform::actor::Event>,
) -> rini::pictures::platform::service::Pictures {
    rini::pictures::platform::service::Pictures::start(vec![
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
    ])
}

/// The config actor, which tells the reactor of each change and the spaces actor of the band the
/// bar keeps clear: that band is the spaces actor's to apply, so it hears of every accepted
/// change directly rather than through the reactor.
fn spawn_config_actor(
    config: &Config,
    events_tx: &reactor::Sender,
    spaces_tx: &rini::displays::platform::spaces::Sender,
    config_path: &std::path::Path,
) -> rini::app::config::actor::Sender {
    ConfigActor::spawn_with_path(
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
        config_path.to_path_buf(),
    )
}

/// The window server's notifications rini follows, and the rest windows' reads of the windows they
/// follow while `following` is on.
fn window_notify(
    events_tx: &reactor::Sender,
    spaces_tx: &rini::displays::platform::spaces::Sender,
    rest_tx: &channels::Sender<rini::decorations::platform::rest::Event>,
    following: rini::displays::event::Following,
    server_reads: std::sync::mpsc::Receiver<rini::displays::event::ToRead>,
) -> window_notify_actor::WindowNotify {
    window_notify_actor::WindowNotify::new(
        events_tx.clone(),
        spaces_tx.clone(),
        &[
            KnownCGSEvent::ManagedSpaceMembershipUpdated,
            KnownCGSEvent::SpaceWindowManagementCapabilitiesChanged,
            KnownCGSEvent::SpaceWindowDestroyed,
            KnownCGSEvent::SpaceWindowCreated,
            // Native focus wakeups. Payload identities are racy, so adjacent
            // events are coalesced before re-querying WindowServer key focus.
            KnownCGSEvent::WindowReordered,
            KnownCGSEvent::WindowUnhidden,
            KnownCGSEvent::WindowHidden,
            KnownCGSEvent::WindowClosed,
            KnownCGSEvent::WindowManagerSpaceFrontConnectionChanged,
            KnownCGSEvent::WindowManagerGlobalFrontConnectionChanged,
            KnownCGSEvent::SpaceCreated,
            KnownCGSEvent::SpaceDestroyed,
            KnownCGSEvent::WindowMoved,
            KnownCGSEvent::WindowResized,
        ],
        // The rest windows follow every decorated window the window server moves, go back above the
        // ones it reorders, hide with the ones it hides or closes, and learn each one they first
        // meet, all read on the frames thread, while `following` is on.
        vec![Box::new({
            let rest_tx = rest_tx.clone();
            move |read| rest_tx.send(rini::decorations::platform::rest::Event::Server(read))
        })],
        following,
        server_reads,
    )
}

/// What the flight engine is made from: its channel, the pictures, the renderer it draws through,
/// and where it places the real windows and tells the bar of a flight.
fn flight_engine_parts(
    rx: channels::Receiver<rini::transitions::platform::engine::Event>,
    transitions_tx: &channels::Sender<rini::transitions::platform::engine::Event>,
    pictures: rini::pictures::platform::service::Pictures,
    render_tx: channels::Sender<rini::animation::platform::renderer::Batch>,
    events_tx: &reactor::Sender,
    bar_tx: &channels::Sender<rini::bar::platform::actor::Event>,
) -> rini::transitions::platform::engine::Parts {
    rini::transitions::platform::engine::Parts {
        rx,
        tx: transitions_tx.clone(),
        pictures,
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
        window_server: rini::windows::platform::window_server::live(),
        capture: Box::new(rini::pictures::platform::skylight::capture_via_framed),
    }
}

/// The event tap, on a dedicated thread with its own CFRunLoop, isolated from main-thread stalls
/// (layout, animation, SLS IPC).
fn spawn_input(event_tap: InputTap) {
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            rini_runloop::executor::Executor::run(event_tap.run());
            panic!("input thread exited");
        })
        .expect("failed to spawn input thread");
}

/// The saved layout, without the saved windows neither the window server nor a running
/// application still has.
fn load_saved_layout(path: std::path::PathBuf) -> anyhow::Result<(LayoutEngine, DisplayMemory)> {
    let window_server = rini::windows::platform::window_server::live();
    LayoutEngine::load_for_startup_restore(
        path,
        |window: WindowId, id| {
            window_server
                .get_window(WindowServerId::new(id))
                .is_some_and(|info| info.pid == window.pid)
        },
        rini::windows::platform::app::is_bundle_running,
    )
}

/// The flight engine, on a thread of its own made from `parts`.
fn spawn_transitions(parts: rini::transitions::platform::engine::Parts) {
    std::thread::Builder::new()
        .name("transitions".into())
        .spawn(move || {
            let engine = rini::transitions::platform::engine::FlightEngine::new(parts);
            Executor::run_interactive(engine.run());
            panic!("transitions thread exited");
        })
        .expect("failed to spawn transitions thread");
}

fn install_panic_hook() {
    // Abort on panic instead of propagating panics to the main thread.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        original_hook(info);
        std::process::abort();
    }));
}
