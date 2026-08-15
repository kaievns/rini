# rini-client

`rini-client` is the Rust client for Rini's JSON-over-Mach IPC API. It is meant
for macOS plugins and companion applications that need to query Rini, execute a
command, or subscribe to Rini events without depending on the window manager.

## Query Rini

```rust
use rini_client::RiniMachClient;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = RiniMachClient::connect()?;
    let workspaces = client.get_workspaces(None)?;
    println!("{workspaces:#?}");
    Ok(())
}
```

Run the complete [query example](examples/query.rs) from the Rini repository:

```sh
cargo run -p rini-client --example query
```

## Listen for events

`subscribe` returns a subscription handle whose `recv_event` method blocks until
Rini publishes the next matching, typed `RiniEvent`:

```rust
use rini_client::{EventKind, RiniMachClient};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = RiniMachClient::connect()?;
    let subscription = client.subscribe(EventKind::WorkspaceChanged)?;

    loop {
        let event = subscription.recv_event()?;
        println!("{}", serde_json::to_string_pretty(&event)?);
    }
}
```

Run the complete [event listener example](examples/listen.rs). It listens for
all events by default, or for the event name passed as its first argument:

```sh
cargo run -p rini-client --example listen
cargo run -p rini-client --example listen -- workspace_changed
```

Supported event names are `workspace_changed`, `windows_changed`,
`window_title_changed`, `focused_window_changed`, and `stacks_changed`. Use `*`
to listen for all events.

For a more complete example, see the [dimmer example](examples/dimmer.rs),
which dims unfocused windows and updates them as Rini events arrive:

```sh
cargo run -p rini-client --example dimmer
```

Set `RINI_BS_NAME` to use a non-default Rini bootstrap service (for example,
when running multiple instances).
