//! Global edit and update handlers.

use axum::{
    extract::{Form, Path, State},
    response::{Html, IntoResponse, Redirect},
    Extension,
};
use std::collections::HashMap;

use anyhow::Context as _;
use crate::admin::AdminState;
use crate::core::auth::{AuthUser, Claims};
use crate::core::field::FieldType;
use crate::core::validate::ValidationError;
use crate::db::{ops, query};
use crate::db::query::AccessResult;
use crate::hooks::lifecycle::{self, HookContext, HookEvent};

/// Extract the user document from AuthUser extension (for access checks).
fn get_user_doc(auth_user: &Option<Extension<AuthUser>>) -> Option<&crate::core::Document> {
    auth_user.as_ref().map(|Extension(au)| &au.user_doc)
}

fn check_global_access_or_forbid(
    state: &AdminState,
    access_ref: Option<&str>,
    auth_user: &Option<Extension<AuthUser>>,
) -> Result<AccessResult, axum::response::Response> {
    let user_doc = get_user_doc(auth_user);
    let conn = state.pool.get()
        .map_err(|_| forbidden(state, "Database error").into_response())?;
    state.hook_runner.check_access(access_ref, user_doc, None, None, &conn)
        .map_err(|e| {
            tracing::error!("Access check error: {}", e);
            forbidden(state, "Access check failed").into_response()
        })
}

fn forbidden(state: &AdminState, message: &str) -> Html<String> {
    let data = serde_json::json!({
        "title": "Forbidden",
        "message": message,
        "collections": state.sidebar_collections(),
        "globals": state.sidebar_globals(),
    });
    match state.render("errors/403", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>403 Forbidden</h1><p>{}</p>", message)),
    }
}

/// GET /admin/globals/{slug} — show edit form for a global
pub async fn edit_form(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(e) => return server_error(&state, &format!("Registry lock poisoned: {}", e)).into_response(),
        };
        match reg.get_global(&slug) {
            Some(d) => d.clone(),
            None => return not_found(&state, &format!("Global '{}' not found", slug)).into_response(),
        }
    };

    // Check read access
    match check_global_access_or_forbid(&state, def.access.read.as_deref(), &auth_user) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to view this global").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let hooks = def.hooks.clone();
    let fields = def.fields.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let read_result = tokio::task::spawn_blocking(move || {
        runner.fire_before_read(&hooks, &slug_owned, "get_global", HashMap::new())?;
        let doc = ops::get_global(&pool, &slug_owned, &def_owned)?;
        let doc = runner.apply_after_read(&hooks, &fields, &slug_owned, "get_global", doc);
        Ok::<_, anyhow::Error>(doc)
    }).await;

    let document = match read_result {
        Ok(Ok(doc)) => doc,
        Ok(Err(e)) => return server_error(&state, &format!("Query error: {}", e)).into_response(),
        Err(e) => return server_error(&state, &format!("Task error: {}", e)).into_response(),
    };

    // Strip field-level read-denied fields
    let mut doc_fields = document.fields.clone();
    {
        let user_doc = get_user_doc(&auth_user);
        if let Ok(conn) = state.pool.get() {
            let denied = state.hook_runner.check_field_read_access(&def.fields, user_doc, &conn);
            for name in &denied {
                doc_fields.remove(name);
            }
        }
    }

    let values: HashMap<String, String> = doc_fields.iter()
        .map(|(k, v)| {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            (k.clone(), s)
        })
        .collect();

    let mut fields = build_field_contexts(&def.fields, &values, &HashMap::new());

    // Enrich relationship fields with options
    enrich_field_contexts(&mut fields, &def.fields, &doc_fields, &state);

    let data = serde_json::json!({
        "page_title": def.display_name(),
        "collections": state.sidebar_collections(),
        "globals": state.sidebar_globals(),
        "global": {
            "slug": def.slug,
            "display_name": def.display_name(),
        },
        "fields": fields,
        "user": claims.as_ref().map(|Extension(c)| serde_json::json!({
            "email": c.email,
            "id": c.sub,
            "collection": c.collection,
        })),
    });

    render_or_error(&state, "globals/edit", &data).into_response()
}

/// POST /admin/globals/{slug} — update a global
pub async fn update_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Form(mut form_data): Form<HashMap<String, String>>,
) -> axum::response::Response {
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(_) => return redirect_response("/admin"),
        };
        reg.get_global(&slug).cloned()
    };
    let def = match def {
        Some(d) => d,
        None => return redirect_response("/admin"),
    };

    // Check update access
    match check_global_access_or_forbid(&state, def.access.update.as_deref(), &auth_user) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to update this global").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    // Strip field-level update-denied fields
    {
        let user_doc = get_user_doc(&auth_user);
        if let Ok(conn) = state.pool.get() {
            let denied = state.hook_runner.check_field_write_access(&def.fields, user_doc, "update", &conn);
            for name in &denied {
                form_data.remove(name);
            }
        }
    }

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let hooks = def.hooks.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let form_data_clone = form_data.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut conn = pool.get().context("DB connection")?;
        let tx = conn.transaction().context("Start transaction")?;

        let hook_ctx = HookContext {
            collection: slug_owned.clone(),
            operation: "update".to_string(),
            data: form_data.iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect(),
        };
        let global_table = format!("_global_{}", slug_owned);
        let final_ctx = runner.run_before_write(
            &hooks, &def_owned.fields, hook_ctx, &tx, &global_table, Some("default"),
        )?;
        let final_data = lifecycle::hook_ctx_to_string_map(&final_ctx);
        let doc = query::update_global(&tx, &slug_owned, &def_owned, &final_data)?;
        tx.commit().context("Commit transaction")?;
        Ok::<_, anyhow::Error>(doc)
    }).await;

    match result {
        Ok(Ok(doc)) => {
            state.hook_runner.fire_after_event(
                &def.hooks, &def.fields, HookEvent::AfterChange,
                slug.clone(), "update".to_string(), doc.fields.clone(),
            );
            redirect_response(&format!("/admin/globals/{}", slug))
        }
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let error_map = ve.to_field_map();
                let fields = build_field_contexts(&def.fields, &form_data_clone, &error_map);
                let data = serde_json::json!({
                    "page_title": def.display_name(),
                    "collections": state.sidebar_collections(),
                    "globals": state.sidebar_globals(),
                    "global": {
                        "slug": def.slug,
                        "display_name": def.display_name(),
                    },
                    "fields": fields,
                });
                html_with_toast(&state, "globals/edit", &data, &e.to_string())
            } else {
                tracing::error!("Global update error: {}", e);
                redirect_response(&format!("/admin/globals/{}", slug))
            }
        }
        Err(e) => {
            tracing::error!("Global update task error: {}", e);
            redirect_response(&format!("/admin/globals/{}", slug))
        }
    }
}

// --- Helpers ---

/// Build field context objects for template rendering.
fn build_field_contexts(
    fields: &[crate::core::field::FieldDefinition],
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
) -> Vec<serde_json::Value> {
    fields.iter().map(|field| {
        let value = values.get(&field.name).cloned().unwrap_or_default();
        let label = field.name.split('_')
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    None => String::new(),
                    Some(f) => f.to_uppercase().chain(c).collect(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        let mut ctx = serde_json::json!({
            "name": field.name,
            "field_type": field.field_type.as_str(),
            "label": label,
            "required": field.required,
            "value": value,
            "placeholder": field.admin.placeholder,
            "description": field.admin.description,
            "readonly": field.admin.readonly,
        });

        if let Some(err) = errors.get(&field.name) {
            ctx["error"] = serde_json::json!(err);
        }

        match &field.field_type {
            FieldType::Select => {
                let options: Vec<_> = field.options.iter().map(|opt| {
                    serde_json::json!({
                        "label": opt.label,
                        "value": opt.value,
                        "selected": opt.value == value,
                    })
                }).collect();
                ctx["options"] = serde_json::json!(options);
            }
            FieldType::Checkbox => {
                let checked = matches!(value.as_str(), "1" | "true" | "on" | "yes");
                ctx["checked"] = serde_json::json!(checked);
            }
            FieldType::Relationship => {
                if let Some(ref rc) = field.relationship {
                    ctx["relationship_collection"] = serde_json::json!(rc.collection);
                    ctx["has_many"] = serde_json::json!(rc.has_many);
                }
            }
            FieldType::Array => {
                let sub_fields: Vec<_> = field.fields.iter().map(|sf| {
                    serde_json::json!({
                        "name": sf.name,
                        "field_type": sf.field_type.as_str(),
                        "label": sf.name.split('_')
                            .map(|w| {
                                let mut c = w.chars();
                                match c.next() {
                                    None => String::new(),
                                    Some(f) => f.to_uppercase().chain(c).collect(),
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" "),
                        "required": sf.required,
                    })
                }).collect();
                ctx["sub_fields"] = serde_json::json!(sub_fields);
            }
            _ => {}
        }

        ctx
    }).collect()
}

/// Enrich field contexts with data that requires DB access (relationship options).
fn enrich_field_contexts(
    fields: &mut [serde_json::Value],
    field_defs: &[crate::core::field::FieldDefinition],
    doc_fields: &HashMap<String, serde_json::Value>,
    state: &AdminState,
) {
    let reg = match state.registry.read() {
        Ok(r) => r,
        Err(_) => return,
    };
    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };

    for (ctx, field_def) in fields.iter_mut().zip(field_defs.iter()) {
        match field_def.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field_def.relationship {
                    if let Some(related_def) = reg.get_collection(&rc.collection) {
                        let title_field = related_def.title_field().map(|s| s.to_string());
                        let find_query = query::FindQuery::default();
                        if let Ok(docs) = query::find(&conn, &rc.collection, related_def, &find_query) {
                            let current_value = ctx.get("value")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let options: Vec<_> = docs.iter().map(|doc| {
                                let label = title_field.as_ref()
                                    .and_then(|f| doc.get_str(f))
                                    .unwrap_or(&doc.id);
                                serde_json::json!({
                                    "value": doc.id,
                                    "label": label,
                                    "selected": doc.id == current_value,
                                })
                            }).collect();
                            ctx["relationship_options"] = serde_json::json!(options);
                        }
                    }
                }
            }
            FieldType::Array => {
                let rows = match doc_fields.get(&field_def.name) {
                    Some(serde_json::Value::Array(arr)) => {
                        arr.iter().enumerate().map(|(idx, row)| {
                            let row_obj = row.as_object();
                            let sub_values: Vec<_> = field_def.fields.iter().map(|sf| {
                                let val = row_obj
                                    .and_then(|m| m.get(&sf.name))
                                    .map(|v| match v {
                                        serde_json::Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    })
                                    .unwrap_or_default();
                                serde_json::json!({
                                    "name": sf.name,
                                    "field_type": sf.field_type.as_str(),
                                    "value": val,
                                    "field_name_indexed": format!("{}[{}][{}]", field_def.name, idx, sf.name),
                                })
                            }).collect();
                            serde_json::json!({
                                "index": idx,
                                "sub_fields": sub_values,
                            })
                        }).collect()
                    }
                    _ => Vec::new(),
                };
                ctx["rows"] = serde_json::json!(rows);
            }
            _ => {}
        }
    }
}

fn redirect_response(url: &str) -> axum::response::Response {
    Redirect::to(url).into_response()
}

fn html_with_toast(state: &AdminState, template: &str, data: &serde_json::Value, toast: &str) -> axum::response::Response {
    match state.render(template, data) {
        Ok(html) => {
            let mut resp = Html(html).into_response();
            if let Ok(val) = toast.parse() {
                resp.headers_mut().insert("X-Crap-Toast", val);
            }
            resp
        }
        Err(e) => Html(format!("<h1>Template Error</h1><pre>{}</pre>", e)).into_response(),
    }
}

fn render_or_error(state: &AdminState, template: &str, data: &serde_json::Value) -> Html<String> {
    match state.render(template, data) {
        Ok(html) => Html(html),
        Err(e) => Html(format!("<h1>Template Error</h1><pre>{}</pre>", e)),
    }
}

fn not_found(state: &AdminState, message: &str) -> Html<String> {
    let data = serde_json::json!({
        "title": "Not Found",
        "message": message,
        "collections": state.sidebar_collections(),
        "globals": state.sidebar_globals(),
    });
    match state.render("errors/404", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>404</h1><p>{}</p>", message)),
    }
}

fn server_error(state: &AdminState, message: &str) -> Html<String> {
    let data = serde_json::json!({
        "title": "Server Error",
        "message": message,
        "collections": state.sidebar_collections(),
        "globals": state.sidebar_globals(),
    });
    match state.render("errors/500", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>500</h1><p>{}</p>", message)),
    }
}
