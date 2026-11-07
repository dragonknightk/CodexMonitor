import { spawn } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { once } from "node:events";
import { mkdtemp, mkdir, readFile, rename, rm, stat } from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { DatabaseSync } from "node:sqlite";

const DASHBOARD_RECORD_PREFIX = "CODEX_MONITOR_INSTALLED_SMOKE_DASHBOARD:";
const DEFAULT_TIMEOUT_MS = 60_000;
const OTLP_ADDRESS = { host: "127.0.0.1", port: 4318 };

class SmokeFailure extends Error {
  constructor(code) {
    super(code);
    this.code = code;
  }
}

function fail(code) {
  throw new SmokeFailure(code);
}

function requireCondition(condition, code) {
  if (!condition) fail(code);
}

function parseArguments(argv) {
  let executable;
  let linuxDegraded = false;
  let requireNotifications = false;
  let timeoutMs = DEFAULT_TIMEOUT_MS;
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--executable") {
      executable = argv[index + 1];
      index += 1;
    } else if (argument === "--linux-degraded") {
      linuxDegraded = true;
    } else if (argument === "--require-notifications") {
      requireNotifications = true;
    } else if (argument === "--timeout-ms") {
      timeoutMs = Number.parseInt(argv[index + 1] ?? "", 10);
      index += 1;
    } else {
      fail("unknown_argument");
    }
  }
  requireCondition(
    typeof executable === "string" && executable.length > 0,
    "missing_executable",
  );
  requireCondition(
    Number.isSafeInteger(timeoutMs) && timeoutMs >= 10_000,
    "invalid_timeout",
  );
  requireCondition(
    !linuxDegraded || process.platform === "linux",
    "degraded_mode_requires_linux",
  );
  requireCondition(
    !requireNotifications || !linuxDegraded,
    "notifications_cannot_be_required_in_degraded_mode",
  );
  return { executable, linuxDegraded, requireNotifications, timeoutMs };
}

function deadlineAfter(timeoutMs) {
  return Date.now() + timeoutMs;
}

async function delay(milliseconds) {
  await new Promise((resolve) => setTimeout(resolve, milliseconds));
}

async function retryUntil(deadline, operation, code) {
  let lastError;
  while (Date.now() < deadline) {
    try {
      const result = await operation();
      if (result !== undefined && result !== false) return result;
    } catch (error) {
      lastError = error;
    }
    await delay(100);
  }
  if (lastError instanceof SmokeFailure) throw lastError;
  fail(code);
}

async function canConnect({ host, port }) {
  return await new Promise((resolve) => {
    const socket = net.createConnection({ host, port });
    const finish = (connected) => {
      socket.destroy();
      resolve(connected);
    };
    socket.setTimeout(500);
    socket.once("connect", () => finish(true));
    socket.once("timeout", () => finish(false));
    socket.once("error", () => finish(false));
  });
}

function createDashboardLaunchQueue(stdout) {
  const values = [];
  const waiters = [];
  let pending = "";
  let terminalError;

  const publish = (url) => {
    const waiter = waiters.shift();
    if (waiter) waiter.resolve(url);
    else values.push(url);
  };

  const reject = (code) => {
    if (terminalError) return;
    terminalError = new SmokeFailure(code);
    for (const waiter of waiters.splice(0)) waiter.reject(terminalError);
  };

  const consumeLine = (line) => {
    if (!line.startsWith(DASHBOARD_RECORD_PREFIX)) return;
    const encoded = line.slice(DASHBOARD_RECORD_PREFIX.length);
    try {
      const url = Buffer.from(encoded, "base64url").toString("utf8");
      const parsed = new URL(url);
      if (parsed.protocol !== "http:" || parsed.hostname !== "127.0.0.1") {
        reject("dashboard_url_boundary");
        return;
      }
      if (
        parsed.pathname !== "/api/v1/session/bootstrap" ||
        !parsed.searchParams.has("token")
      ) {
        reject("dashboard_url_shape");
        return;
      }
      publish(url);
    } catch {
      reject("dashboard_record_invalid");
    }
  };

  stdout.setEncoding("utf8");
  stdout.on("data", (chunk) => {
    pending += chunk;
    if (pending.length > 64 * 1024) {
      reject("resident_stdout_overflow");
      pending = "";
      return;
    }
    for (;;) {
      const newline = pending.indexOf("\n");
      if (newline < 0) break;
      const line = pending.slice(0, newline).replace(/\r$/, "");
      pending = pending.slice(newline + 1);
      consumeLine(line);
    }
  });

  return {
    async next(deadline) {
      if (terminalError) throw terminalError;
      if (values.length > 0) return values.shift();
      const remaining = deadline - Date.now();
      requireCondition(remaining > 0, "dashboard_launch_timeout");
      return await new Promise((resolve, rejectPromise) => {
        const waiter = { resolve, reject: rejectPromise };
        const timer = setTimeout(() => {
          const index = waiters.indexOf(waiter);
          if (index >= 0) waiters.splice(index, 1);
          rejectPromise(new SmokeFailure("dashboard_launch_timeout"));
        }, remaining);
        waiter.resolve = (value) => {
          clearTimeout(timer);
          resolve(value);
        };
        waiters.push(waiter);
      });
    },
  };
}

function collectSafeStderr(stderr) {
  let pending = "";
  const codes = [];
  stderr.setEncoding("utf8");
  stderr.on("data", (chunk) => {
    pending = `${pending}${chunk}`.slice(-16 * 1024);
    const lines = pending.split(/\r?\n/);
    pending = lines.pop() ?? "";
    for (const line of lines) {
      if (/^codex_monitor_[a-z0-9_:]+$/.test(line)) codes.push(line);
    }
    if (codes.length > 32) codes.splice(0, codes.length - 32);
  });
  return codes;
}

function spawnApplication(
  executable,
  arguments_,
  environment,
  stdio = ["ignore", "ignore", "pipe"],
) {
  const child = spawn(executable, arguments_, {
    env: environment,
    stdio,
    windowsHide: true,
  });
  child.once("error", () => {});
  return child;
}

async function waitForChildExit(child, deadline, timeoutCode) {
  if (child.exitCode !== null)
    return { code: child.exitCode, signal: child.signalCode };
  const remaining = deadline - Date.now();
  requireCondition(remaining > 0, timeoutCode);
  return await Promise.race([
    once(child, "exit").then(([code, signal]) => ({ code, signal })),
    delay(remaining).then(() => fail(timeoutCode)),
  ]);
}

async function launchSecondary(executable, arguments_, environment, deadline) {
  const child = spawnApplication(executable, arguments_, environment);
  const result = await waitForChildExit(
    child,
    deadline,
    "secondary_instance_timeout",
  ).catch((error) => {
    child.kill();
    throw error;
  });
  requireCondition(
    result.code === 0 && result.signal === null,
    "secondary_instance_failed",
  );
}

async function boundedResponseBytes(response, maximum, code) {
  const declaredLength = response.headers.get("content-length");
  if (declaredLength !== null) {
    requireCondition(/^\d+$/u.test(declaredLength), `${code}_declared_size`);
    const parsedLength = Number(declaredLength);
    requireCondition(
      Number.isSafeInteger(parsedLength) &&
        parsedLength > 0 &&
        parsedLength <= maximum,
      `${code}_declared_size`,
    );
  }
  const bytes = new Uint8Array(await response.arrayBuffer());
  requireCondition(
    bytes.byteLength > 0 && bytes.byteLength <= maximum,
    `${code}_actual_size`,
  );
  return bytes;
}

async function verifyDashboardDocument(origin, cookie) {
  const response = await fetch(`${origin}/`, {
    headers: { Cookie: cookie },
    redirect: "manual",
  });
  requireCondition(response.status === 200, "dashboard_document_status");
  requireCondition(
    response.headers.get("content-type")?.startsWith("text/html") === true,
    "dashboard_document_content_type",
  );
  requireCondition(
    response.headers.get("cache-control") === "no-store",
    "dashboard_document_cache_control",
  );
  requireCondition(
    response.headers.get("x-content-type-options") === "nosniff",
    "dashboard_document_nosniff",
  );
  requireCondition(
    response.headers
      .get("content-security-policy")
      ?.includes("default-src 'self'") === true,
    "dashboard_document_csp",
  );
  const documentBytes = await boundedResponseBytes(
    response,
    1024 * 1024,
    "dashboard_document",
  );
  let documentText;
  try {
    documentText = new TextDecoder("utf-8", { fatal: true }).decode(
      documentBytes,
    );
  } catch {
    throw new SmokeFailure("dashboard_document_encoding");
  }
  requireCondition(
    documentText.includes('<div id="root"></div>'),
    "dashboard_root_missing",
  );

  const references = [
    ...documentText.matchAll(/\b(?:src|href)="([^"]+)"/gu),
  ].map((match) => match[1]);
  requireCondition(references.length >= 2, "dashboard_assets_missing");
  requireCondition(
    references.every((reference) => reference.startsWith("/assets/")),
    "dashboard_asset_reference_boundary",
  );

  for (const reference of new Set(references)) {
    const assetUrl = new URL(reference, origin);
    requireCondition(
      assetUrl.origin === origin &&
        assetUrl.search === "" &&
        assetUrl.hash === "",
      "dashboard_asset_origin_boundary",
    );
    const asset = await fetch(assetUrl, {
      headers: { Cookie: cookie },
      redirect: "manual",
    });
    requireCondition(asset.status === 200, "dashboard_asset_status");
    const contentType = asset.headers.get("content-type") ?? "";
    if (assetUrl.pathname.endsWith(".js")) {
      requireCondition(
        contentType.includes("javascript"),
        "dashboard_script_content_type",
      );
    } else if (assetUrl.pathname.endsWith(".css")) {
      requireCondition(
        contentType.startsWith("text/css"),
        "dashboard_style_content_type",
      );
    } else {
      requireCondition(
        contentType !== "" && !contentType.startsWith("text/html"),
        "dashboard_asset_content_type",
      );
    }
    requireCondition(
      asset.headers.get("cache-control") ===
        "public, max-age=31536000, immutable",
      "dashboard_asset_cache_control",
    );
    requireCondition(
      asset.headers.get("x-content-type-options") === "nosniff",
      "dashboard_asset_nosniff",
    );
    await boundedResponseBytes(asset, 16 * 1024 * 1024, "dashboard_asset");
  }
}

async function bootstrapSession(launchUrl) {
  const response = await fetch(launchUrl, { redirect: "manual" });
  requireCondition(response.status === 303, "bootstrap_status");
  requireCondition(
    response.headers.get("location") === "/",
    "bootstrap_redirect",
  );
  const setCookie = response.headers.get("set-cookie");
  requireCondition(typeof setCookie === "string", "bootstrap_cookie_missing");
  requireCondition(
    /;\s*HttpOnly(?:;|$)/i.test(setCookie),
    "bootstrap_cookie_not_http_only",
  );
  requireCondition(
    /;\s*SameSite=Strict(?:;|$)/i.test(setCookie),
    "bootstrap_cookie_not_strict",
  );
  const cookie = setCookie.split(";", 1)[0];
  const origin = new URL(launchUrl).origin;
  const session = await authenticatedJson(origin, "/api/v1/session", cookie);
  requireCondition(
    session.body.authenticated === true,
    "session_not_authenticated",
  );
  requireCondition(
    session.body.csrf?.header_name === "X-CodexMonitor-CSRF",
    "csrf_header_invalid",
  );
  requireCondition(
    typeof session.body.csrf?.proof === "string",
    "csrf_proof_missing",
  );
  await verifyDashboardDocument(origin, cookie);
  return { origin, cookie, csrf: session.body.csrf.proof };
}

async function authenticatedJson(origin, route, cookie, init = {}) {
  const response = await fetch(`${origin}${route}`, {
    ...init,
    headers: { Cookie: cookie, ...(init.headers ?? {}) },
  });
  requireCondition(response.ok, `api_status_${response.status}`);
  return { response, body: await response.json() };
}

async function replaceSettings(session, settings, etag) {
  const result = await authenticatedJson(
    session.origin,
    "/api/v1/settings",
    session.cookie,
    {
      method: "PUT",
      headers: {
        "Content-Type": "application/json",
        "If-Match": etag,
        Origin: session.origin,
        "X-CodexMonitor-CSRF": session.csrf,
      },
      body: JSON.stringify(settings),
    },
  );
  const nextEtag = result.response.headers.get("etag");
  requireCondition(typeof nextEtag === "string", "settings_etag_missing");
  return { document: result.body, etag: nextEtag };
}

function encodeVarint(value) {
  let remaining = BigInt(value);
  requireCondition(remaining >= 0n, "protobuf_negative_varint");
  const bytes = [];
  do {
    let byte = Number(remaining & 0x7fn);
    remaining >>= 7n;
    if (remaining !== 0n) byte |= 0x80;
    bytes.push(byte);
  } while (remaining !== 0n);
  return Buffer.from(bytes);
}

function protobufBytes(field, value) {
  return Buffer.concat([
    encodeVarint((field << 3) | 2),
    encodeVarint(value.length),
    value,
  ]);
}

function protobufString(field, value) {
  return protobufBytes(field, Buffer.from(value, "utf8"));
}

function protobufFixed64(field, value) {
  const bytes = Buffer.allocUnsafe(8);
  bytes.writeBigUInt64LE(BigInt(value));
  return Buffer.concat([encodeVarint((field << 3) | 1), bytes]);
}

function anyString(value) {
  return protobufString(1, value);
}

function keyValue(key, value) {
  return Buffer.concat([
    protobufString(1, key),
    protobufBytes(2, anyString(value)),
  ]);
}

function createBinaryOtlpFixture(identity) {
  const model = `synthetic-smoke-${identity}`;
  const responseId = `synthetic-response-${identity}`;
  const conversationId = `synthetic-conversation-${identity}`;
  const threadId = `synthetic-thread-${identity}`;
  const body = JSON.stringify({
    type: "response.completed",
    response: {
      id: responseId,
      model,
      usage: {
        input_tokens: 1200,
        input_tokens_details: { cached_tokens: 400 },
        output_tokens: 160,
        output_tokens_details: { reasoning_tokens: 516 },
      },
    },
  });
  const attributes = [
    ["event.name", "codex.sse_event"],
    ["event.kind", "response.completed"],
    ["conversation.id", conversationId],
    ["thread.id", threadId],
    ["model", model],
    ["model_reasoning_effort", "high"],
  ];
  const record = Buffer.concat([
    protobufFixed64(1, BigInt(Date.now()) * 1_000_000n),
    protobufBytes(5, anyString(body)),
    ...attributes.map(([key, value]) => protobufBytes(6, keyValue(key, value))),
  ]);
  const scope = protobufString(1, "synthetic-codex-monitor-installed-smoke");
  const scopeLogs = Buffer.concat([
    protobufBytes(1, scope),
    protobufBytes(2, record),
  ]);
  const resource = Buffer.concat([
    protobufBytes(1, keyValue("service.name", "synthetic-codex-client")),
    protobufBytes(1, keyValue("codex.surface", "synthetic-desktop")),
  ]);
  const resourceLogs = Buffer.concat([
    protobufBytes(1, resource),
    protobufBytes(2, scopeLogs),
  ]);
  return { body: protobufBytes(1, resourceLogs), model };
}

async function sendBinaryOtlp(fixture) {
  const response = await fetch(
    `http://${OTLP_ADDRESS.host}:${OTLP_ADDRESS.port}/v1/logs`,
    {
      method: "POST",
      headers: { "Content-Type": "application/x-protobuf" },
      body: fixture.body,
    },
  );
  requireCondition(response.status === 200, `otlp_status_${response.status}`);
  const contentType = response.headers.get("content-type") ?? "";
  requireCondition(
    contentType.startsWith("application/x-protobuf"),
    "otlp_response_encoding",
  );
}

async function verifyProjectedAlert(
  session,
  model,
  deadline,
  expectedActionState,
) {
  return await retryUntil(
    deadline,
    async () => {
      const calls = await authenticatedJson(
        session.origin,
        `/api/v1/model-calls?model=${encodeURIComponent(model)}&limit=10`,
        session.cookie,
      );
      const call = calls.body.items?.find((item) => item.model === model);
      if (
        !call ||
        call.tokens?.reasoning !== 516 ||
        call.alert_state !== "matched"
      )
        return false;
      const alerts = await authenticatedJson(
        session.origin,
        `/api/v1/alerts?model_call_id=${encodeURIComponent(call.id)}&limit=10`,
        session.cookie,
      );
      const alert = alerts.body.items?.find(
        (item) => item.model_call_id === call.id,
      );
      if (
        alert?.evidence?.reasoning_output_tokens !== 516 ||
        alert?.evidence?.pattern_n !== 1 ||
        (expectedActionState !== undefined &&
          alert.action?.state !== expectedActionState)
      ) {
        return false;
      }
      return { call, alert };
    },
    "projection_or_alert_timeout",
  );
}

async function verifyHistoricalImport(session, model, deadline) {
  return await retryUntil(
    deadline,
    async () => {
      const calls = await authenticatedJson(
        session.origin,
        `/api/v1/model-calls?model=${encodeURIComponent(model)}&limit=10`,
        session.cookie,
      );
      const call = calls.body.items?.find((item) => item.model === model);
      if (
        !call ||
        call.tokens?.reasoning !== 516 ||
        call.temporal_class !== "historical" ||
        call.alert_state !== "none"
      ) {
        return false;
      }
      const alerts = await authenticatedJson(
        session.origin,
        `/api/v1/alerts?model_call_id=${encodeURIComponent(call.id)}&limit=10`,
        session.cookie,
      );
      return alerts.body.items?.length === 0 ? call : false;
    },
    "legacy_import_projection_timeout",
  );
}

function createSyntheticLegacyDatabase(file) {
  const database = new DatabaseSync(file);
  const timestamp = new Date().toISOString();
  database.exec(`
    PRAGMA user_version = 1;
    CREATE TABLE codex_token_events (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      created_at TEXT NOT NULL,
      received_at TEXT NOT NULL,
      conversation_id TEXT,
      thread_id TEXT,
      model TEXT,
      reasoning_effort TEXT,
      input_tokens INTEGER,
      cached_input_tokens INTEGER,
      output_tokens INTEGER,
      reasoning_output_tokens INTEGER,
      raw_event_name TEXT,
      stream_event_kind TEXT,
      surface TEXT,
      client TEXT
    );
    CREATE TABLE codex_conversations (
      conversation_id TEXT PRIMARY KEY,
      first_seen_at TEXT NOT NULL,
      last_seen_at TEXT NOT NULL,
      model TEXT,
      reasoning_effort TEXT,
      surface TEXT,
      client TEXT,
      raw_event_name TEXT
    );
    CREATE INDEX idx_codex_token_events_created_at ON codex_token_events(created_at);
    CREATE INDEX idx_codex_token_events_thread_id ON codex_token_events(thread_id);
    CREATE INDEX idx_codex_token_events_conversation_id ON codex_token_events(conversation_id);
    CREATE INDEX idx_codex_conversations_model ON codex_conversations(model);
    INSERT INTO codex_conversations (
      conversation_id, first_seen_at, last_seen_at, model,
      reasoning_effort, surface, client, raw_event_name
    ) VALUES (
      'synthetic-legacy-conversation', '${timestamp}', '${timestamp}',
      'synthetic-legacy-upgrade-v1', 'high', 'desktop',
      'codex-cli', 'codex.conversation_starts'
    );
    INSERT INTO codex_token_events (
      created_at, received_at, conversation_id, thread_id, model,
      reasoning_effort, input_tokens, cached_input_tokens, output_tokens,
      reasoning_output_tokens, raw_event_name, stream_event_kind, surface, client
    ) VALUES (
      '${timestamp}', '${timestamp}', 'synthetic-legacy-conversation',
      'synthetic-legacy-thread', 'synthetic-legacy-upgrade-v1', 'high',
      1200, 400, 160, 516, 'codex.sse_event', 'response.completed',
      'desktop', 'codex-cli'
    );
  `);
  database.close();
  return "synthetic-legacy-upgrade-v1";
}

async function fileSha256(file) {
  return createHash("sha256")
    .update(await readFile(file))
    .digest("hex");
}

async function createIsolatedEnvironment(linuxDegraded) {
  const root = await mkdtemp(
    path.join(os.tmpdir(), "codex-monitor-installed-smoke-"),
  );
  const home = path.join(root, "home");
  const local = path.join(root, "local");
  const xdgData = path.join(root, "xdg-data");
  const xdgConfig = path.join(root, "xdg-config");
  await Promise.all(
    [home, local, xdgData, xdgConfig].map((directory) =>
      mkdir(directory, { recursive: true }),
    ),
  );
  const environment = {
    ...process.env,
    CI: "true",
    CODEX_MONITOR_INSTALLED_SMOKE: "1",
    HOME: home,
    USERPROFILE: home,
    LOCALAPPDATA: local,
    XDG_DATA_HOME: xdgData,
    XDG_CONFIG_HOME: xdgConfig,
  };
  if (linuxDegraded)
    environment.CODEX_MONITOR_INSTALLED_SMOKE_LINUX_DEGRADED = "1";
  const dataDirectory =
    process.platform === "win32"
      ? path.join(local, "CodexMonitor")
      : process.platform === "darwin"
        ? path.join(home, "Library", "Application Support", "CodexMonitor")
        : path.join(xdgData, "codex-monitor");
  return {
    root,
    environment,
    database: path.join(dataDirectory, "codex-monitor.sqlite3"),
    legacyDatabase: path.join(root, "synthetic-legacy.sqlite3"),
  };
}

async function verifyInstalledExecutable(executable) {
  requireCondition(path.isAbsolute(executable), "executable_path_not_absolute");
  const metadata = await stat(executable).catch(() =>
    fail("installed_executable_missing"),
  );
  requireCondition(metadata.isFile(), "installed_executable_not_file");
  const segments = path
    .normalize(executable)
    .split(path.sep)
    .map((segment) => segment.toLowerCase());
  requireCondition(
    !segments.includes("target"),
    "cargo_output_is_not_installed_artifact",
  );
}

async function assertDatabaseReleased(database) {
  const metadata = await stat(database);
  requireCondition(
    metadata.isFile() && metadata.size > 0,
    "sqlite_projection_file_missing",
  );
  if (process.platform === "win32") {
    const moved = `${database}.released`;
    await rename(database, moved).catch(() =>
      fail("sqlite_writer_not_released"),
    );
    await rename(moved, database);
  }
}

async function run() {
  const options = parseArguments(process.argv.slice(2));
  await verifyInstalledExecutable(options.executable);
  const isolated = await createIsolatedEnvironment(options.linuxDegraded);
  const deadline = deadlineAfter(options.timeoutMs);
  let resident;
  let firstSession;
  let initialSettings;
  let currentEtag;
  let settingsRestored = false;
  let safeStderr = [];

  try {
    const legacyModel = createSyntheticLegacyDatabase(isolated.legacyDatabase);
    const legacySourceBefore = await fileSha256(isolated.legacyDatabase);
    resident = spawnApplication(
      options.executable,
      ["--background"],
      isolated.environment,
      ["ignore", "pipe", "pipe"],
    );
    const dashboardLaunches = createDashboardLaunchQueue(resident.stdout);
    safeStderr = collectSafeStderr(resident.stderr);

    await retryUntil(
      deadline,
      async () => {
        requireCondition(
          resident.exitCode === null,
          "resident_exited_during_startup",
        );
        return await canConnect(OTLP_ADDRESS);
      },
      "otlp_listener_start_timeout",
    );

    await launchSecondary(
      options.executable,
      ["--import-legacy", isolated.legacyDatabase],
      isolated.environment,
      deadline,
    );
    const firstLaunchUrl = await dashboardLaunches.next(deadline);
    firstSession = await bootstrapSession(firstLaunchUrl);
    await verifyHistoricalImport(firstSession, legacyModel, deadline);
    requireCondition(
      (await fileSha256(isolated.legacyDatabase)) === legacySourceBefore,
      "legacy_source_was_modified",
    );

    const readCapabilities = async () =>
      await authenticatedJson(
        firstSession.origin,
        "/api/v1/capabilities",
        firstSession.cookie,
      );
    const capabilities = options.requireNotifications
      ? await retryUntil(
          deadline,
          async () => {
            const observed = await readCapabilities();
            return observed.body.platform?.system_notifications === "available"
              ? observed
              : false;
          },
          "required_notification_capability_unavailable",
        )
      : await readCapabilities();
    requireCondition(
      capabilities.body.api_version === "v1",
      "capabilities_version",
    );
    requireCondition(
      capabilities.body.platform?.login_autostart === true,
      "autostart_unavailable",
    );
    if (options.linuxDegraded) {
      requireCondition(
        capabilities.body.platform?.tray === "unavailable",
        "linux_tray_not_degraded",
      );
      requireCondition(
        capabilities.body.platform?.system_notifications === "unavailable",
        "linux_notification_not_degraded",
      );
    } else {
      requireCondition(
        capabilities.body.platform?.tray === "available",
        "tray_unavailable",
      );
      requireCondition(
        ["available", "permission_required", "unavailable"].includes(
          capabilities.body.platform?.system_notifications,
        ),
        "notification_capability_invalid",
      );
      if (options.requireNotifications) {
        requireCondition(
          capabilities.body.platform?.system_notifications === "available",
          "required_notification_capability_unavailable",
        );
      }
    }

    const settings = await authenticatedJson(
      firstSession.origin,
      "/api/v1/settings",
      firstSession.cookie,
      { headers: { "Accept-Language": "en-US" } },
    );
    initialSettings = structuredClone(settings.body.settings);
    currentEtag = settings.response.headers.get("etag");
    requireCondition(typeof currentEtag === "string", "settings_etag_missing");
    requireCondition(
      initialSettings.login_autostart === false,
      "autostart_not_disabled_by_default",
    );

    const toggledNotifications = !initialSettings.system_notifications;
    const enabledAutostart = await replaceSettings(
      firstSession,
      {
        ...initialSettings,
        login_autostart: true,
        system_notifications: toggledNotifications,
      },
      currentEtag,
    );
    requireCondition(
      enabledAutostart.document.settings.login_autostart === true,
      "autostart_enable_failed",
    );
    requireCondition(
      enabledAutostart.document.settings.system_notifications ===
        toggledNotifications,
      "notification_toggle_failed",
    );
    currentEtag = enabledAutostart.etag;

    const disabledAutostart = await replaceSettings(
      firstSession,
      {
        ...initialSettings,
        login_autostart: false,
        system_notifications: false,
      },
      currentEtag,
    );
    requireCondition(
      disabledAutostart.document.settings.login_autostart === false,
      "autostart_disable_failed",
    );
    currentEtag = disabledAutostart.etag;

    if (options.requireNotifications) {
      const notificationsEnabled = await replaceSettings(
        firstSession,
        {
          ...initialSettings,
          login_autostart: false,
          system_notifications: true,
        },
        currentEtag,
      );
      requireCondition(
        notificationsEnabled.document.settings.system_notifications === true,
        "notification_enable_failed",
      );
      currentEtag = notificationsEnabled.etag;
    }

    // The first Dashboard client is now treated as closed. Ingestion continues
    // before a second manual launch obtains an independent browser session.
    const identity = randomBytes(8).toString("hex");
    const fixture = createBinaryOtlpFixture(identity);
    await sendBinaryOtlp(fixture);

    await launchSecondary(
      options.executable,
      [],
      isolated.environment,
      deadline,
    );
    const secondLaunchUrl = await dashboardLaunches.next(deadline);
    requireCondition(
      secondLaunchUrl !== firstLaunchUrl,
      "second_instance_reused_bootstrap",
    );
    const secondSession = await bootstrapSession(secondLaunchUrl);
    await verifyProjectedAlert(
      secondSession,
      fixture.model,
      deadline,
      options.requireNotifications ? "completed" : undefined,
    );

    const currentSettings = await authenticatedJson(
      secondSession.origin,
      "/api/v1/settings",
      secondSession.cookie,
    );
    currentEtag = currentSettings.response.headers.get("etag");
    requireCondition(typeof currentEtag === "string", "settings_etag_missing");
    await replaceSettings(secondSession, initialSettings, currentEtag);
    settingsRestored = true;

    await launchSecondary(
      options.executable,
      ["--quit"],
      isolated.environment,
      deadline,
    );
    const residentExit = await waitForChildExit(
      resident,
      deadline,
      "ordered_quit_timeout",
    );
    requireCondition(
      residentExit.code === 0 && residentExit.signal === null,
      "ordered_quit_failed",
    );
    requireCondition(
      !(await canConnect(OTLP_ADDRESS)),
      "otlp_listener_not_released",
    );
    await assertDatabaseReleased(isolated.database);
    requireCondition(
      !safeStderr.some(
        (code) =>
          code.includes("shutdown_issue") ||
          code.includes("shutdown_wait_timed_out"),
      ),
      "shutdown_diagnostic_reported",
    );

    process.stdout.write(
      `${JSON.stringify({
        schema_version: 1,
        installed_artifact: true,
        binary_otlp: true,
        legacy_upgrade_import: true,
        sqlite_projection: true,
        dashboard_bootstrap: true,
        dashboard_document: true,
        dashboard_assets: true,
        reasoning_516_alert: true,
        login_autostart: true,
        second_instance: true,
        browser_independence: true,
        notification_toggle: true,
        notification_available: options.requireNotifications,
        notification_dispatch: options.requireNotifications,
        tray: options.linuxDegraded ? "degraded" : "available",
        ordered_quit: true,
      })}\n`,
    );
  } finally {
    if (resident && resident.exitCode === null) {
      if (!settingsRestored && firstSession && initialSettings && currentEtag) {
        await replaceSettings(firstSession, initialSettings, currentEtag).catch(
          () => {},
        );
      }
      const cleanupDeadline = deadlineAfter(10_000);
      await launchSecondary(
        options.executable,
        ["--quit"],
        isolated.environment,
        cleanupDeadline,
      ).catch(() => {});
      await waitForChildExit(
        resident,
        cleanupDeadline,
        "cleanup_timeout",
      ).catch(() => {
        resident.kill();
      });
    }
    await rm(isolated.root, { recursive: true, force: true }).catch(() => {});
  }
}

try {
  await run();
} catch (error) {
  const code =
    error instanceof SmokeFailure ? error.code : "unexpected_failure";
  process.stderr.write(`installed_artifact_smoke_failed:${code}\n`);
  process.exitCode = 1;
}
