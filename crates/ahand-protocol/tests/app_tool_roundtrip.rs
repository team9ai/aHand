//! Wire round-trip tests for app tool messages (Envelope tags 35/36/37).

use ahand_protocol::{
    AppToolDescriptor, AppToolError, AppToolRequest, AppToolResponse, AppToolsUpdate, Envelope,
    app_tool_response, envelope,
};
use prost::Message;

fn roundtrip(payload: envelope::Payload) -> envelope::Payload {
    let env = Envelope {
        device_id: "dev-1".into(),
        msg_id: "m-1".into(),
        payload: Some(payload),
        ..Default::default()
    };
    let bytes = env.encode_to_vec();
    Envelope::decode(bytes.as_slice()).unwrap().payload.unwrap()
}

#[test]
fn app_tools_update_roundtrips() {
    let update = AppToolsUpdate {
        revision: 7,
        tools: vec![AppToolDescriptor {
            name: "list_documents".into(),
            description: "List open documents".into(),
            input_schema_json: r#"{"type":"object","properties":{}}"#.into(),
            requires_approval: true,
        }],
    };
    match roundtrip(envelope::Payload::AppToolsUpdate(update.clone())) {
        envelope::Payload::AppToolsUpdate(got) => assert_eq!(got, update),
        other => panic!("wrong payload variant: {other:?}"),
    }
}

#[test]
fn app_tool_request_roundtrips() {
    let req = AppToolRequest {
        tool_call_id: "call-1".into(),
        name: "list_documents".into(),
        args_json: r#"{"limit":5}"#.into(),
        timeout_ms: 30_000,
    };
    match roundtrip(envelope::Payload::AppToolRequest(req.clone())) {
        envelope::Payload::AppToolRequest(got) => assert_eq!(got, req),
        other => panic!("wrong payload variant: {other:?}"),
    }
}

#[test]
fn app_tool_response_error_roundtrips() {
    let resp = AppToolResponse {
        tool_call_id: "call-1".into(),
        result: Some(app_tool_response::Result::Error(AppToolError {
            code: "TOOL_NOT_FOUND".into(),
            message: "no such tool".into(),
        })),
    };
    match roundtrip(envelope::Payload::AppToolResponse(resp.clone())) {
        envelope::Payload::AppToolResponse(got) => assert_eq!(got, resp),
        other => panic!("wrong payload variant: {other:?}"),
    }
}

#[test]
fn app_tool_response_success_roundtrips() {
    let resp = AppToolResponse {
        tool_call_id: "call-2".into(),
        result: Some(app_tool_response::Result::ResultJson(
            r#"{"documents":["a.txt","b.txt"]}"#.into(),
        )),
    };
    match roundtrip(envelope::Payload::AppToolResponse(resp.clone())) {
        envelope::Payload::AppToolResponse(got) => assert_eq!(got, resp),
        other => panic!("wrong payload variant: {other:?}"),
    }
}
