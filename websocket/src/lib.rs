use rrplug::prelude::*;

use std::{collections::HashMap, str::FromStr, sync::Arc, time::Duration};

use tokio::{net::TcpStream, time::timeout};

use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{HeaderName, HeaderValue},
        Message,
    },
    MaybeTlsStream, WebSocketStream,
};

use futures_util::stream::SplitSink;
use futures_util::{sink::SinkExt, stream::StreamExt};
use lazy_static::lazy_static;
use std::sync::Mutex;
use tokio::runtime::Runtime;

struct WebSocketContainer {
    write: Arc<Mutex<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>>,
}

lazy_static! {
    static ref STREAM_MAP: Arc<Mutex<HashMap<String, WebSocketContainer>>> =
        Arc::new(Mutex::new(HashMap::new()));
    static ref RT: Runtime = tokio::runtime::Runtime::new().unwrap();
    static ref LAST_MESSAGE: Arc<Mutex<HashMap<String, Vec<String>>>> =
        Arc::new(Mutex::new(HashMap::new()));
}

#[derive(Debug)]
pub struct WebsocketPlugin {}

impl Plugin for WebsocketPlugin {
    fn new(plugin_data: &PluginData) -> Self {
        _ = plugin_data.register_sq_functions(sq_connect_to_server);
        _ = plugin_data.register_sq_functions(sq_disconnect_from_server);
        _ = plugin_data.register_sq_functions(sq_write_message);
        _ = plugin_data.register_sq_functions(get_last_messages);
        _ = plugin_data.register_sq_functions(get_open_connections);

        Self {}
    }
}

entry!(WebsocketPlugin);

#[rrplug::sqfunction(VM = "Server", ExportName = "PL_ConnectToWebsocket")]
fn sq_connect_to_server(
    socket_name: String,
    url: String,
    headers: String,
    connection_time_out: i32,
    keep_alive: bool,
) -> bool {
    log::info!("Trying to establish websocket connection [{socket_name}] to [{url}]");

    let mut open_new_socket = true;

    if STREAM_MAP.lock().unwrap().contains_key(&socket_name) {
        if keep_alive {
            log::info!("There is still a open websocket connection for [{socket_name}] keeping already existing socket.");
            open_new_socket = false;
        } else {
            log::warn!(
                "There is still a open websocket connection for [{socket_name}] closing websocket."
            );
            disconnect_from_server(&socket_name);
        }
    }

    let mut was_success = true;
    if open_new_socket {
        was_success = RT.block_on(connect_to_server(
            socket_name,
            url,
            headers,
            connection_time_out as u64,
        ));
    }

    Ok(was_success)
}

#[rrplug::sqfunction(VM = "Server", ExportName = "PL_DisconnectFromWebsocket")]
fn sq_disconnect_from_server(socket_name: String) {
    log::info!("Disconnecting websocket client [{socket_name}]");

    disconnect_from_server(&socket_name);

    Ok(())
}

#[rrplug::sqfunction(VM = "Server", ExportName = "PL_WriteToWebsocket")]
fn sq_write_message(socket_name: String, message: String) -> bool {
    log::trace!("Writing to websocket [{socket_name}] message [{message}]");

    let write_successfully = RT.block_on(write_message(&socket_name, message));

    if !write_successfully {
        disconnect_from_server(&socket_name);
    }

    Ok(write_successfully)
}

type VecString = Vec<String>; // seams to be a quirk of the new proc macro will fix soon :|

#[rrplug::sqfunction(VM = "Server", ExportName = "PL_ReadFromWebsocket")]
fn get_last_messages(socket_name: String) -> VecString {
    log::trace!("Trying to read from the websocket [{socket_name}] buffer");

    let mut last_message_map = LAST_MESSAGE.lock().unwrap();
    let lock = last_message_map
        .get(&socket_name.clone())
        .unwrap()
        .to_vec()
        .clone();
    last_message_map.get_mut(&socket_name).unwrap().clear();

    Ok(lock)
}

#[rrplug::sqfunction(VM = "Server", ExportName = "PL_GetOpenWebsockets")]
fn get_open_connections() -> VecString {
    let keys = STREAM_MAP
        .lock()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<String>>();

    Ok(keys)
}

async fn write_message(socket_name: &String, message: String) -> bool {
    // Retrieve the map
    let map_lock = STREAM_MAP.lock().unwrap();

    // Get the WebSocketContainer from the map
    if let Some(container) = map_lock.get(socket_name) {
        // Access the write field of the WebSocketContainer
        let mut write_mutex = container.write.lock().unwrap();
        let write = &mut *write_mutex;

        // Send the message
        match write.send(Message::Text(message.clone())).await {
            Ok(_) => {
                log::trace!("Message for [{socket_name}] was sent successfully [{message}]");
            }
            Err(_) => {
                log::warn!("Failed to write the message to [{socket_name}]");
                return false;
            }
        }
        return true;
    } else {
        // Handle the case when the WebSocketContainer is not found
        log::warn!("There is no established connection for [{socket_name}]");
        return false;
    }
}

fn disconnect_from_server(socket_name: &String) {
    match RT.block_on(
        STREAM_MAP
            .lock()
            .unwrap()
            .get(socket_name)
            .unwrap()
            .write
            .lock()
            .unwrap()
            .close(),
    ) {
        Ok(_) => {
            log::info!("Websocket [{socket_name}] closed successfully");
        }
        Err(_) => {
            log::warn!("There was an issue closing the websocket [{socket_name}]");
        }
    }

    STREAM_MAP.lock().unwrap().remove(socket_name);
}

async fn connect_to_server(
    socket_name: String,
    url_string: String,
    headers: String,
    connection_time_out: u64,
) -> bool {
    log::debug!("Trying to establish websocket connection [{socket_name}]...");

    let header: Vec<&str> = headers.split("|#!#|").collect();

    let can_connect: bool;

    log::debug!("Config: [{socket_name}] url = [{url_string}]");
    let mut request = url_string.clone().into_client_request().unwrap();

    let headers = request.headers_mut();

    log::debug!("Config: [{socket_name}] parsing headers...");
    for (header, value) in header
        .iter()
        .step_by(2)
        .zip(header.iter().skip(1).step_by(2))
    {
        let header_name = HeaderName::from_str(header).unwrap();
        let header_value = HeaderValue::from_str(value).unwrap();

        log::debug!("Config: [{socket_name}] Adding header [{header}] value: [{value}]");

        headers.insert(header_name, header_value);
    }

    log::debug!(
        "Config: [{socket_name}] connection timeout [{}s]",
        connection_time_out
    );
    let timeout_duration = Duration::from_secs(connection_time_out); // Set the desired timeout duration

    let connect_result = timeout(timeout_duration, connect_async(request)).await;

    match connect_result {
        Ok(Ok(socket_stream)) => {
            log::info!("Connection successful for [{url_string}]");

            let (stream_stuff, _response) = socket_stream;

            let (split_write, split_read) = stream_stuff.split();

            let new_container = WebSocketContainer {
                write: Arc::new(Mutex::new(split_write)),
            };

            STREAM_MAP
                .lock()
                .unwrap()
                .insert(socket_name.clone(), new_container);
            LAST_MESSAGE
                .lock()
                .unwrap()
                .insert(socket_name.clone(), Vec::new());

            let socket_name_arc = Arc::new(socket_name.clone());

            tokio::spawn(async move {
                log::info!("Spinning up listening thread for [{socket_name}]");

                let socket_name_arc = socket_name_arc.clone();

                let mut read_stream = split_read;

                while let Some(result) = read_stream.next().await {
                    match result {
                        Err(_) => log::warn!("Websocket [{socket_name}] closed unexpectedly"),
                        Ok(message) => {
                            if message.is_text() {
                                let s = message
                                    .into_text()
                                    .expect("Websocket provided invalid string format");
                                log::trace!(
                                    "Received message from Websocket [{:?}] message [{:?}]",
                                    socket_name_arc.clone(),
                                    s.clone()
                                );

                                let lock = {
                                    let socket_name_str = socket_name_arc.as_str();
                                    let last_message_map = LAST_MESSAGE.lock().unwrap();
                                    let mut lock =
                                        last_message_map.get(socket_name_str).unwrap().clone();
                                    lock.push(s.clone());
                                    lock
                                };

                                let mut last_message_map = LAST_MESSAGE.lock().unwrap();
                                last_message_map.insert(socket_name_arc.as_str().to_string(), lock);
                            } else if message.is_binary() {
                                log::warn!("Unparseable Binary message received from Websocket [{:?}] data [{:?}]", socket_name_arc.clone(), message.into_data());
                            } else if message.is_ping() {
                                log::debug!(
                                    "Ping message received from Websocket [{:?}]",
                                    socket_name_arc.clone()
                                );
                            } else if message.is_pong() {
                                log::debug!(
                                    "Pong message received from Websocket [{:?}]",
                                    socket_name_arc.clone()
                                );
                            } else if message.is_close() {
                                log::info!(
                                    "Close message received from Websocket [{:?}]",
                                    socket_name_arc.clone()
                                );
                                break;
                            } else {
                                log::warn!(
                                    "Single Websocket Frame detected from Websocket [{:?}]",
                                    socket_name_arc.clone()
                                );
                            }
                        }
                    }
                }
            });
            can_connect = true;
        }
        Ok(Err(e)) => {
            log::error!("Failed to connect to {socket_name} reason: {:#?}", e);
            can_connect = false;
        }
        Err(_) => {
            log::error!("Timeout was reached while trying to connect to [{socket_name}]");
            can_connect = false;
        }
    }

    can_connect
}

impl Drop for WebsocketPlugin {
    fn drop(&mut self) {
        for (key, _) in &*STREAM_MAP.lock().unwrap() {
            disconnect_from_server(key)
        }
    }
}
