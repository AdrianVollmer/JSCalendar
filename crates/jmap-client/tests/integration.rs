use jmap_client::client::{Client, Credentials};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn session_mock(server: &MockServer) {
    let body = json!({
        "capabilities": {
            "urn:ietf:params:jmap:core": {},
            "urn:ietf:params:jmap:calendars": {},
            "urn:ietf:params:jmap:contacts": {},
        },
        "accounts": {
            "a1": {
                "name": "user@example.com",
                "isPersonal": true,
                "isReadOnly": false,
                "accountCapabilities": {
                    "urn:ietf:params:jmap:calendars": {},
                    "urn:ietf:params:jmap:contacts": {}
                }
            }
        },
        "primaryAccounts": {
            "urn:ietf:params:jmap:calendars": "a1",
            "urn:ietf:params:jmap:contacts": "a1"
        },
        "username": "user@example.com",
        "apiUrl": format!("{}/api", server.uri()),
        "downloadUrl": format!("{}/download", server.uri()),
        "uploadUrl": format!("{}/upload", server.uri()),
        "state": "abc123",
    });
    Mock::given(method("GET"))
        .and(path("/.well-known/jmap"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn connected_client(server: &MockServer) -> Client {
    session_mock(server).await;
    let mut client = Client::new(
        format!("{}/.well-known/jmap", server.uri())
            .parse()
            .unwrap(),
        Credentials::Basic {
            username: "user@example.com".into(),
            password: "hunter2".into(),
        },
    );
    client.connect().await.expect("connect should succeed");
    client
}

#[tokio::test]
async fn connects_and_resolves_calendars_account() {
    let server = MockServer::start().await;
    let client = connected_client(&server).await;
    assert_eq!(client.calendars_account_id().unwrap(), "a1");
    assert_eq!(client.session().unwrap().username, "user@example.com");
}

#[tokio::test]
async fn unauthorized_session_maps_to_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/jmap"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let mut client = Client::new(
        format!("{}/.well-known/jmap", server.uri())
            .parse()
            .unwrap(),
        Credentials::Basic {
            username: "u".into(),
            password: "wrong".into(),
        },
    );
    let err = client.connect().await.unwrap_err();
    assert!(matches!(err, jmap_client::Error::Unauthorized));
}

#[tokio::test]
async fn get_calendars_parses_list() {
    let server = MockServer::start().await;
    let client = connected_client(&server).await;

    let body = json!({
        "methodResponses": [
            ["Calendar/get", {
                "accountId": "a1",
                "state": "s1",
                "list": [
                    {
                        "id": "cal1",
                        "name": "Personal",
                        "color": "#3b82f6",
                        "sortOrder": 0,
                        "isSubscribed": true,
                        "isVisible": true,
                        "myRights": {
                            "mayReadItems": true, "mayAddItems": true,
                            "mayUpdateAll": true, "mayRemoveAll": true,
                            "mayReadFreeBusy": true, "mayUpdateOwn": true,
                            "mayUpdatePrivate": true, "mayRemoveOwn": true,
                            "mayAdmin": false, "mayDelete": false
                        }
                    }
                ],
                "notFound": []
            }, "c0"]
        ],
        "sessionState": "abc123"
    });
    Mock::given(method("POST"))
        .and(path("/api"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let calendars = client.get_calendars("a1").await.unwrap();
    assert_eq!(calendars.len(), 1);
    assert_eq!(calendars[0].name, "Personal");
    assert_eq!(calendars[0].color.as_deref(), Some("#3b82f6"));
}

#[tokio::test]
async fn query_events_chains_query_and_get() {
    let server = MockServer::start().await;
    let client = connected_client(&server).await;

    let body = json!({
        "methodResponses": [
            ["CalendarEvent/query", {
                "accountId": "a1",
                "queryState": "q1",
                "canCalculateChanges": false,
                "position": 0,
                "ids": ["evt1"],
                "total": 1
            }, "q0"],
            ["CalendarEvent/get", {
                "accountId": "a1",
                "state": "s1",
                "list": [
                    {
                        "@type": "Event",
                        "id": "evt1",
                        "uid": "evt1-uid",
                        "title": "Standup",
                        "start": "2026-07-06T09:00:00",
                        "timeZone": "Europe/Berlin",
                        "duration": "PT30M",
                        "calendarIds": { "cal1": true }
                    }
                ],
                "notFound": []
            }, "g0"]
        ],
        "sessionState": "abc123"
    });
    Mock::given(method("POST"))
        .and(path("/api"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let events = client
        .query_events(
            "a1",
            Some("cal1"),
            Some("2026-07-01T00:00:00"),
            Some("2026-08-01T00:00:00"),
        )
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].title.as_deref(), Some("Standup"));
    assert_eq!(events[0].duration, "PT30M");
    assert!(events[0]
        .calendar_ids
        .as_ref()
        .unwrap()
        .contains_key("cal1"));
}

#[tokio::test]
async fn create_event_merges_server_assigned_fields() {
    let server = MockServer::start().await;
    let client = connected_client(&server).await;

    let body = json!({
        "methodResponses": [
            ["CalendarEvent/set", {
                "accountId": "a1",
                "oldState": "s0",
                "newState": "s1",
                "created": {
                    "new": { "id": "evt42" }
                }
            }, "c0"]
        ],
        "sessionState": "abc123"
    });
    Mock::given(method("POST"))
        .and(path("/api"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let mut event = jmap_client::jscalendar::CalendarEvent::new(
        "evt42-uid",
        jmap_client::jscalendar::LocalDateTime("2026-07-10T10:00:00".into()),
    );
    event.title = Some("Dentist".into());
    let created = client.create_event("a1", &event).await.unwrap();
    assert_eq!(created.id.as_deref(), Some("evt42"));
    assert_eq!(created.title.as_deref(), Some("Dentist"));
}

#[tokio::test]
async fn destroy_event_reports_protocol_error_on_failure() {
    let server = MockServer::start().await;
    let client = connected_client(&server).await;

    let body = json!({
        "methodResponses": [
            ["CalendarEvent/set", {
                "accountId": "a1",
                "oldState": "s0",
                "newState": "s0",
                "notDestroyed": {
                    "evt1": { "type": "notFound" }
                }
            }, "c0"]
        ],
        "sessionState": "abc123"
    });
    Mock::given(method("POST"))
        .and(path("/api"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let err = client.destroy_event("a1", "evt1").await.unwrap_err();
    assert!(matches!(err, jmap_client::Error::Protocol(_)));
}

#[tokio::test]
async fn contacts_account_resolves_and_cards_parse_with_birthday() {
    let server = MockServer::start().await;
    let client = connected_client(&server).await;
    assert_eq!(client.contacts_account_id().as_deref(), Some("a1"));

    let body = json!({
        "methodResponses": [
            ["ContactCard/get", {
                "accountId": "a1",
                "state": "s1",
                "list": [
                    {
                        "@type": "Card",
                        "version": "1.0",
                        "id": "card1",
                        "uid": "card1-uid",
                        "name": { "@type": "Name", "full": "Ada Lovelace" },
                        "anniversaries": {
                            "a1": {
                                "@type": "Anniversary",
                                "kind": "birth",
                                "date": { "@type": "PartialDate", "month": 12, "day": 10 }
                            }
                        },
                        "addressBookIds": { "ab1": true }
                    }
                ],
                "notFound": []
            }, "c0"]
        ],
        "sessionState": "abc123"
    });
    Mock::given(method("POST"))
        .and(path("/api"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let cards = client.get_contact_cards("a1", None).await.unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].display_name(), "Ada Lovelace");

    let occs = jmap_client::jscontact::expand_birthdays(
        &cards,
        chrono::NaiveDate::from_ymd_opt(2026, 12, 1).unwrap(),
        chrono::NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(),
    );
    assert_eq!(occs.len(), 1);
    assert_eq!(occs[0].name, "Ada Lovelace");
}

#[tokio::test]
async fn create_contact_card_merges_server_assigned_fields() {
    let server = MockServer::start().await;
    let client = connected_client(&server).await;

    let body = json!({
        "methodResponses": [
            ["ContactCard/set", {
                "accountId": "a1",
                "oldState": "s0",
                "newState": "s1",
                "created": {
                    "new": { "id": "card42" }
                }
            }, "c0"]
        ],
        "sessionState": "abc123"
    });
    Mock::given(method("POST"))
        .and(path("/api"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let mut card = jmap_client::jscontact::Card::new("card42-uid");
    card.name = Some(jmap_client::jscontact::NameProperty {
        full: Some("Grace Hopper".into()),
        ..Default::default()
    });
    let created = client.create_contact_card("a1", &card).await.unwrap();
    assert_eq!(created.id.as_deref(), Some("card42"));
    assert_eq!(created.display_name(), "Grace Hopper");
}

#[tokio::test]
async fn destroy_contact_card_reports_protocol_error_on_failure() {
    let server = MockServer::start().await;
    let client = connected_client(&server).await;

    let body = json!({
        "methodResponses": [
            ["ContactCard/set", {
                "accountId": "a1",
                "oldState": "s0",
                "newState": "s0",
                "notDestroyed": {
                    "card1": { "type": "notFound" }
                }
            }, "c0"]
        ],
        "sessionState": "abc123"
    });
    Mock::given(method("POST"))
        .and(path("/api"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let err = client
        .destroy_contact_card("a1", "card1")
        .await
        .unwrap_err();
    assert!(matches!(err, jmap_client::Error::Protocol(_)));
}
