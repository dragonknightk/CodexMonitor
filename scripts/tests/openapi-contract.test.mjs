import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import YAML from "yaml";

import { openApiPath } from "../check-generated-api.mjs";

const HTTP_METHODS = new Set([
  "delete",
  "get",
  "head",
  "options",
  "patch",
  "post",
  "put",
  "trace",
]);

const documentPromise = readFile(openApiPath, "utf8").then((source) =>
  YAML.parse(source),
);

function resolveRef(document, value) {
  let resolved = value;
  const seen = new Set();

  while (resolved?.$ref) {
    const reference = resolved.$ref;
    assert.match(
      reference,
      /^#\//,
      `external reference is not allowed: ${reference}`,
    );
    assert.equal(seen.has(reference), false, `cyclic reference: ${reference}`);
    seen.add(reference);
    resolved = reference
      .slice(2)
      .split("/")
      .map((part) => part.replaceAll("~1", "/").replaceAll("~0", "~"))
      .reduce((current, part) => current?.[part], document);
    assert.ok(resolved, `unresolved reference: ${reference}`);
  }

  return resolved;
}

function operations(document) {
  return Object.entries(document.paths).flatMap(([path, pathItem]) =>
    Object.entries(pathItem)
      .filter(([method]) => HTTP_METHODS.has(method))
      .map(([method, operation]) => ({ method, operation, path })),
  );
}

function parameterKeys(document, operation) {
  return operation.parameters
    .map((parameter) => resolveRef(document, parameter))
    .map((parameter) => `${parameter.in}:${parameter.name}`)
    .sort();
}

function assertMembers(actual, expected, label) {
  assert.deepEqual([...actual].sort(), [...expected].sort(), label);
}

function assertClosedRequired(document, schemaName, required) {
  const schema = document.components.schemas[schemaName];
  assert.equal(schema.type, "object", `${schemaName} must be an object`);
  assert.equal(
    schema.additionalProperties,
    false,
    `${schemaName} must reject unknown fields`,
  );
  assertMembers(
    schema.required,
    required,
    `${schemaName} required fields drifted`,
  );
}

function allowsNull(schema) {
  return (
    (Array.isArray(schema.type) && schema.type.includes("null")) ||
    schema.oneOf?.some((branch) => branch.type === "null") === true
  );
}

test("OpenAPI defines the complete versioned operation authority", async () => {
  const document = await documentPromise;
  const expectedOperations = [
    ["/api/v1/session/bootstrap", "get", "bootstrapSession"],
    ["/api/v1/session", "get", "getCurrentSession"],
    ["/api/v1/health", "get", "getHealth"],
    ["/api/v1/capabilities", "get", "getCapabilities"],
    ["/api/v1/model-calls", "get", "listModelCalls"],
    ["/api/v1/conversations", "get", "listConversations"],
    ["/api/v1/summaries", "get", "getModelCallSummary"],
    ["/api/v1/sources", "get", "listSources"],
    ["/api/v1/sources/{source_id}/retry", "post", "retrySource"],
    ["/api/v1/alerts", "get", "listAlerts"],
    ["/api/v1/settings", "get", "getSettings"],
    ["/api/v1/settings", "put", "replaceSettings"],
    ["/api/v1/events", "get", "streamEvents"],
  ];
  const actualOperations = operations(document);

  assert.equal(document.openapi, "3.1.0");
  assert.equal(
    document.jsonSchemaDialect,
    "https://json-schema.org/draft/2020-12/schema",
  );
  assert.deepEqual(document.servers, [
    { description: "Current authenticated Dashboard origin", url: "/" },
  ]);
  assert.deepEqual(document.security, [{ localSession: [] }]);
  assert.equal(
    document["x-codex-monitor-security"].network_boundary,
    "loopback_only",
  );
  assert.equal(
    document["x-codex-monitor-security"].accepted_host,
    "current_listener_authority",
  );
  assert.equal(document["x-codex-monitor-security"].cors, "disabled");
  assert.deepEqual(
    document["x-codex-monitor-security"].state_changing_methods,
    ["DELETE", "PATCH", "POST", "PUT"],
  );
  assert.deepEqual(document["x-codex-monitor-validation-order"], [
    "host",
    "route",
    "session",
    "origin",
    "csrf",
    "media_type",
    "body_size",
    "schema",
    "handler",
  ]);
  assert.equal(
    document["x-codex-monitor-security"].max_json_body_bytes,
    16_384,
  );

  assertMembers(
    actualOperations.map(({ method, operation, path }) =>
      [path, method, operation.operationId].join(" "),
    ),
    expectedOperations.map((item) => item.join(" ")),
    "operation matrix drifted",
  );
  assert.equal(
    new Set(actualOperations.map(({ operation }) => operation.operationId))
      .size,
    actualOperations.length,
    "operationId values must be unique",
  );
  assert.equal(
    actualOperations.every(({ path }) => path.startsWith("/api/v1/")),
    true,
  );

  const bootstrap = document.paths["/api/v1/session/bootstrap"].get;
  assert.deepEqual(bootstrap.security, []);
  for (const { operation } of actualOperations.filter(
    ({ operation }) => operation !== bootstrap,
  )) {
    assert.equal(
      operation.security,
      undefined,
      "protected operation bypasses security",
    );
  }
});

test("success responses expose the exact DTOs and cache boundary", async () => {
  const document = await documentPromise;
  const expectedJsonResponses = [
    ["/api/v1/session", "get", "200", "CurrentSession"],
    ["/api/v1/health", "get", "200", "HealthSnapshot"],
    ["/api/v1/capabilities", "get", "200", "Capabilities"],
    ["/api/v1/model-calls", "get", "200", "ModelCallPage"],
    ["/api/v1/conversations", "get", "200", "ConversationPage"],
    ["/api/v1/summaries", "get", "200", "ModelCallSummary"],
    ["/api/v1/sources", "get", "200", "SourceList"],
    ["/api/v1/sources/{source_id}/retry", "post", "202", "SourceHealth"],
    ["/api/v1/alerts", "get", "200", "AlertPage"],
    ["/api/v1/settings", "get", "200", "SettingsDocument"],
    ["/api/v1/settings", "put", "200", "SettingsDocument"],
  ];

  for (const [path, method, status, schemaName] of expectedJsonResponses) {
    const response = document.paths[path][method].responses[status];
    assert.equal(
      response.content["application/json"].schema.$ref,
      `#/components/schemas/${schemaName}`,
      `${method.toUpperCase()} ${path} ${status} DTO drifted`,
    );
    assert.equal(
      response.headers["Cache-Control"].$ref,
      "#/components/headers/NoStore",
    );
  }

  for (const method of ["get", "put"]) {
    assert.equal(
      document.paths["/api/v1/settings"][method].responses["200"].headers.ETag
        .$ref,
      "#/components/headers/SettingsRevision",
    );
  }

  const bootstrapResponse =
    document.paths["/api/v1/session/bootstrap"].get.responses["303"];
  assert.deepEqual(Object.keys(bootstrapResponse.headers).sort(), [
    "Cache-Control",
    "Location",
    "Referrer-Policy",
    "Set-Cookie",
  ]);
  assert.equal(bootstrapResponse.content, undefined);

  const streamResponse = document.paths["/api/v1/events"].get.responses["200"];
  assert.equal(
    streamResponse.headers["Cache-Control"].schema.const,
    "no-cache",
  );
  assert.equal(streamResponse.headers["X-Accel-Buffering"].schema.const, "no");
  assert.equal(
    streamResponse.content["text/event-stream"].schema.type,
    "string",
  );
});

test("session, Host, CSRF and optimistic-concurrency contracts are explicit", async () => {
  const document = await documentPromise;
  const securityScheme = document.components.securitySchemes.localSession;
  assert.deepEqual(
    {
      in: securityScheme.in,
      name: securityScheme.name,
      type: securityScheme.type,
    },
    { in: "cookie", name: "codex_monitor_session", type: "apiKey" },
  );

  for (const { operation, path } of operations(document).filter(
    ({ operation }) => operation.operationId !== "bootstrapSession",
  )) {
    assert.equal(
      operation.responses["401"].$ref,
      "#/components/responses/Unauthorized",
    );
    assert.equal(
      resolveRef(document, operation.responses["403"])[
        "x-error-codes"
      ].includes("forbidden_host"),
      true,
      `${path} must specify Host rejection`,
    );
    assert.equal(
      operation.responses["503"].$ref,
      "#/components/responses/ServiceUnavailable",
    );
    assert.equal(operation.responses["500"], undefined);
  }

  const retry = document.paths["/api/v1/sources/{source_id}/retry"].post;
  const replaceSettings = document.paths["/api/v1/settings"].put;
  assertMembers(parameterKeys(document, retry), [
    "header:X-CodexMonitor-CSRF",
    "path:source_id",
  ]);
  assertMembers(parameterKeys(document, replaceSettings), [
    "header:If-Match",
    "header:X-CodexMonitor-CSRF",
  ]);
  assert.equal(
    retry.responses["403"].$ref,
    "#/components/responses/RequestRejected",
  );
  assert.equal(
    retry.responses["400"].$ref,
    "#/components/responses/ValidationFailed",
  );
  assert.equal(
    replaceSettings.responses["403"].$ref,
    "#/components/responses/RequestRejected",
  );
  const settingsRequestBody = resolveRef(document, replaceSettings.requestBody);
  assert.equal(settingsRequestBody.required, true);
  assertMembers(
    Object.keys(settingsRequestBody.content),
    ["application/json"],
    "settings replacement must accept only application/json",
  );
  assert.equal(
    settingsRequestBody.content["application/json"].schema.$ref,
    "#/components/schemas/Settings",
  );
  assert.equal(
    replaceSettings.responses["400"].$ref,
    "#/components/responses/ValidationFailed",
  );
  assert.equal(
    replaceSettings.responses["412"].$ref,
    "#/components/responses/RevisionConflict",
  );
  assert.equal(
    replaceSettings.responses["413"].$ref,
    "#/components/responses/PayloadTooLarge",
  );
  assert.equal(
    replaceSettings.responses["415"].$ref,
    "#/components/responses/UnsupportedMediaType",
  );
  assert.equal(replaceSettings["x-codex-monitor-max-body-bytes"], 16_384);
  assert.equal(
    replaceSettings.responses["428"].$ref,
    "#/components/responses/PreconditionRequired",
  );
  assert.equal(
    document.components.parameters.IfMatchRevision.schema.pattern,
    '^"[A-Za-z0-9_-]{1,128}"$',
  );
  assert.equal(
    document.components.responses.Unauthorized.headers["WWW-Authenticate"].$ref,
    "#/components/headers/SessionChallenge",
  );
  assert.equal(
    document.components.responses.BootstrapRejected.headers["WWW-Authenticate"]
      .$ref,
    "#/components/headers/BootstrapChallenge",
  );
  const bootstrap = document.paths["/api/v1/session/bootstrap"].get;
  assert.equal(
    bootstrap.responses["403"].$ref,
    "#/components/responses/BootstrapHostRejected",
  );
  assert.equal(
    bootstrap.responses["503"].$ref,
    "#/components/responses/BootstrapServiceUnavailable",
  );
  for (const status of ["303", "401", "403", "503"]) {
    assert.equal(
      resolveRef(document, bootstrap.responses[status]).headers[
        "Referrer-Policy"
      ].$ref,
      "#/components/headers/NoReferrer",
      `bootstrap ${status} can leak its token through referrers`,
    );
  }
  assert.deepEqual(
    document.components.responses.BootstrapHostRejected["x-error-codes"],
    ["forbidden_host"],
  );
  assert.deepEqual(
    document.components.responses.BootstrapServiceUnavailable["x-error-codes"],
    ["internal_unavailable"],
  );
  assert.equal(
    document.components.responses.BootstrapServiceUnavailable.content[
      "application/json"
    ].schema.$ref,
    "#/components/schemas/BootstrapServiceUnavailableErrorResponse",
  );

  for (const { method, operation, path } of operations(document)) {
    for (const [status, response] of Object.entries(operation.responses)) {
      const headers = Object.keys(
        resolveRef(document, response).headers ?? {},
      ).map((name) => name.toLowerCase());
      assert.equal(
        headers.some((name) => name.startsWith("access-control-")),
        false,
        `${method.toUpperCase()} ${path} ${status} exposes a CORS response header`,
      );
    }
  }
});

test("error responses have closed safe parameters and status-specific codes", async () => {
  const document = await documentPromise;
  const expectedResponseCodes = {
    BootstrapRejected: ["bootstrap_invalid"],
    HostRejected: ["forbidden_host"],
    InvalidQuery: ["invalid_cursor", "invalid_time_range", "validation_failed"],
    PayloadTooLarge: ["payload_too_large"],
    PreconditionRequired: ["precondition_required"],
    RequestRejected: ["csrf_invalid", "forbidden_host", "forbidden_origin"],
    RevisionConflict: ["revision_conflict"],
    ServiceUnavailable: ["internal_unavailable", "storage_unavailable"],
    SourceNotFound: ["source_not_found"],
    SourceRetryConflict: ["source_retry_unavailable"],
    Unauthorized: ["unauthorized"],
    UnsupportedMediaType: ["unsupported_media_type"],
    ValidationFailed: ["validation_failed"],
  };

  for (const [name, codes] of Object.entries(expectedResponseCodes)) {
    const response = document.components.responses[name];
    assertMembers(
      response["x-error-codes"],
      codes,
      `${name} error codes drifted`,
    );
    assert.equal(
      response.headers["Cache-Control"].$ref,
      "#/components/headers/NoStore",
    );
    assert.equal(
      response.content["application/json"].schema.$ref,
      `#/components/schemas/${name}ErrorResponse`,
    );
    const codeConstraint =
      document.components.schemas[`${name}ErrorResponse`].allOf[1].properties
        .error.allOf[1].properties.code;
    assertMembers(
      codeConstraint.enum ?? [codeConstraint.const],
      codes,
      `${name} payload schema does not enforce x-error-codes`,
    );
  }

  const mappedCodes = new Set(
    Object.values(expectedResponseCodes).flatMap((codes) => codes),
  );
  assertMembers(
    mappedCodes,
    document.components.schemas.ApiErrorCode.enum,
    "ApiErrorCode contains an unmapped value",
  );

  const errorObject = resolveRef(
    document,
    document.components.schemas.ErrorResponse.properties.error,
  );
  assertMembers(Object.keys(errorObject.properties), ["code", "params"]);
  assert.equal(errorObject.additionalProperties, false);
  assert.equal(
    errorObject.properties.params.$ref,
    "#/components/schemas/ErrorParameters",
  );

  const parameters = document.components.schemas.ErrorParameters;
  assert.equal(parameters.additionalProperties, false);
  assertMembers(Object.keys(parameters.properties), [
    "cursor_reason",
    "issues",
    "retry_reason",
  ]);
  assert.equal(document.components.schemas.ErrorParameterValue, undefined);

  for (const schemaName of [
    "ErrorParameters",
    "ValidationField",
    "ValidationIssue",
  ]) {
    const serialized = JSON.stringify(document.components.schemas[schemaName]);
    for (const forbidden of ["message", "path", "payload", "raw", "stack"])
      assert.equal(
        serialized.includes(`\"${forbidden}\"`),
        false,
        `${schemaName} exposes free-form ${forbidden}`,
      );
  }
});

test("list queries use bounded deterministic cursor pagination", async () => {
  const document = await documentPromise;
  const expectedParameters = {
    "/api/v1/alerts": [
      "query:action_state",
      "query:conversation",
      "query:cursor",
      "query:from",
      "query:limit",
      "query:model_call_id",
      "query:to",
    ],
    "/api/v1/conversations": [
      "query:cursor",
      "query:from",
      "query:limit",
      "query:query",
      "query:source_id",
      "query:to",
    ],
    "/api/v1/model-calls": [
      "query:alert",
      "query:client",
      "query:conversation",
      "query:cursor",
      "query:from",
      "query:limit",
      "query:model",
      "query:sort",
      "query:source_id",
      "query:temporal_class",
      "query:to",
    ],
    "/api/v1/summaries": [
      "query:alert",
      "query:client",
      "query:conversation",
      "query:from",
      "query:model",
      "query:source_id",
      "query:temporal_class",
      "query:to",
    ],
  };

  for (const [path, expected] of Object.entries(expectedParameters)) {
    assertMembers(parameterKeys(document, document.paths[path].get), expected);
  }

  const pageLimit = document.components.parameters.PageLimit.schema;
  assert.deepEqual(
    {
      default: pageLimit.default,
      maximum: pageLimit.maximum,
      minimum: pageLimit.minimum,
    },
    { default: 25, maximum: 100, minimum: 1 },
  );
  const cursorDescription = document.components.parameters.Cursor.description;
  for (const phrase of ["exclusive", "endpoint", "filters", "sort"])
    assert.equal(cursorDescription.includes(phrase), true);

  const sort = document.components.parameters.ModelCallSort;
  assert.equal(sort.schema.default, "event_at_desc");
  assert.equal(sort.description.includes("tie-breaker"), true);
  assert.equal(
    sort.description.includes("unknown token values sort last"),
    true,
  );

  const page = document.components.schemas.PageMetadata;
  assert.equal(page.oneOf.length, 2);
  assert.equal(page.oneOf[0].properties.has_more.const, true);
  assert.equal(
    page.oneOf[0].properties.next_cursor.$ref,
    "#/components/schemas/PageCursor",
  );
  assert.equal(page.oneOf[1].properties.has_more.const, false);
  assert.equal(page.oneOf[1].properties.next_cursor.type, "null");

  assert.equal(
    document.paths["/api/v1/conversations"].get.description.includes(
      "last_observed_at descending, then id descending",
    ),
    true,
  );
  assert.equal(
    document.paths["/api/v1/alerts"].get.description.includes(
      "created_at descending, then id descending",
    ),
    true,
  );
});

test("resource DTOs are closed, complete and aligned with domain enums", async () => {
  const document = await documentPromise;
  const requiredProperties = {
    Alert: [
      "action",
      "conversation_id",
      "created_at",
      "evidence",
      "id",
      "model_call_id",
      "rule_id",
      "rule_version",
    ],
    AlertAction: [
      "action_type",
      "completed_at",
      "created_at",
      "error_code",
      "not_before",
      "started_at",
      "state",
      "suppression_reason",
    ],
    AlertEvidence: [
      "event_at",
      "observed_at",
      "pattern_n",
      "quality",
      "reasoning_output_tokens",
      "source_contract_version",
      "source_id",
    ],
    Capabilities: [
      "api_version",
      "event_types",
      "locales",
      "platform",
      "query",
      "rules",
      "sources",
    ],
    Conversation: [
      "alert_count",
      "first_observed_at",
      "id",
      "last_observed_at",
      "model_call_count",
      "resolved_display_name",
      "source_id",
      "thread_id",
    ],
    HealthSnapshot: [
      "api_version",
      "checked_at",
      "dashboard_listener",
      "status",
      "stream_cursor",
    ],
    ModelCall: [
      "alert_count",
      "alert_state",
      "client",
      "conversation",
      "event_at",
      "field_sources",
      "id",
      "model",
      "reasoning_effort",
      "source_ids",
      "surface",
      "temporal_class",
      "tokens",
      "turn_id",
    ],
    ModelCallSummary: [
      "alert_count",
      "cached_input",
      "call_count",
      "input",
      "output",
      "reasoning",
      "total",
    ],
    RuleCapability: [
      "enabled",
      "last_error_code",
      "last_evaluated_at",
      "notify_max_n",
      "rule_id",
      "rule_version",
    ],
    Settings: [
      "language",
      "login_autostart",
      "notify_max_n",
      "retention_days",
      "system_notifications",
    ],
    SourceCapability: [
      "checkpoint_capability",
      "contract_version",
      "source_id",
      "supported_fields",
      "supports_retry",
    ],
    SourceHealth: [
      "last_accepted_at",
      "last_error_code",
      "last_error_occurred_at",
      "receiver",
      "source_contract_version",
      "source_id",
      "status",
      "supported_fields",
      "updated_at",
    ],
    TokenUsage: ["cached_input", "input", "output", "reasoning", "total"],
  };

  for (const [schemaName, required] of Object.entries(requiredProperties))
    assertClosedRequired(document, schemaName, required);

  assertMembers(document.components.schemas.CheckpointCapability.enum, [
    "monotonic_file_offset_v1",
    "none",
  ]);
  assertMembers(document.components.schemas.RuleErrorCode.enum, [
    "internal_queue_closed",
    "persistence_commit_failed",
  ]);
  assertMembers(document.components.schemas.ActionSuppressionReason.enum, [
    "above_threshold",
    "coalesced",
    "historical_event",
    "notifications_disabled",
    "permission_denied",
    "pre_outbox_upgrade",
  ]);

  const modelCall = document.components.schemas.ModelCall.properties;
  for (const field of [
    "client",
    "conversation",
    "model",
    "reasoning_effort",
    "surface",
    "turn_id",
  ])
    assert.equal(
      allowsNull(modelCall[field]),
      true,
      `${field} must expose unknown`,
    );

  const alertAction = document.components.schemas.AlertAction.properties;
  for (const field of [
    "completed_at",
    "error_code",
    "started_at",
    "suppression_reason",
  ])
    assert.equal(
      allowsNull(alertAction[field]),
      true,
      `${field} must be nullable`,
    );

  assert.equal(
    allowsNull(document.components.schemas.Settings.properties.language),
    false,
    "persisted language must be resolved",
  );

  const forbiddenProperties = new Set([
    "account_id",
    "api_key",
    "encrypted_reasoning",
    "hidden_reasoning",
    "local_path",
    "payload",
    "prompt",
    "prompt_text",
    "raw_body",
    "raw_json",
    "reasoning_summary",
    "response_text",
    "session_title",
    "tool_input",
    "tool_output",
    "workspace_path",
  ]);
  const propertyNames = new Set();
  const visit = (value) => {
    if (Array.isArray(value)) {
      value.forEach(visit);
      return;
    }
    if (!value || typeof value !== "object") return;
    if (value.properties)
      Object.keys(value.properties).forEach((name) => propertyNames.add(name));
    Object.values(value).forEach(visit);
  };
  visit(document.components.schemas);
  for (const forbidden of forbiddenProperties)
    assert.equal(
      propertyNames.has(forbidden),
      false,
      `forbidden DTO field: ${forbidden}`,
    );
});

test("SSE contract names every incremental event and requires resynchronization", async () => {
  const document = await documentPromise;
  const operation = document.paths["/api/v1/events"].get;
  const stream = operation.responses["200"].content["text/event-stream"];

  assertMembers(parameterKeys(document, operation), ["query:after_event_id"]);
  assert.equal(
    operation.description.includes("full REST resynchronization"),
    true,
  );
  for (const reason of ["missed_event", "overflow", "server_restart"])
    assert.equal(
      operation.description.includes(reason),
      true,
      `SSE operation description omitted ${reason}`,
    );
  assert.deepEqual(stream["x-sse-event-schemas"], {
    alert: { $ref: "#/components/schemas/AlertEvent" },
    "model-call": { $ref: "#/components/schemas/ModelCallEvent" },
    settings: { $ref: "#/components/schemas/SettingsEvent" },
    "source-health": { $ref: "#/components/schemas/SourceHealthEvent" },
    "stream-control": { $ref: "#/components/schemas/StreamControlEvent" },
  });
  assertMembers(
    document.components.schemas.StreamControlEvent.properties.reason.enum,
    ["missed_event", "overflow", "server_restart"],
  );
});
