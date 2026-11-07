import type { SyntheticEvent } from "react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  Locale,
  Settings,
  SettingsDocument,
  SettingsEvent,
} from "../../api/types";
import type { ResourceSynchronization } from "../events";
import dashboardI18n from "../../i18n";
import type { DisplayError } from "../../shared/errors";
import { errorTranslationKey, toDisplayError } from "../../shared/errors";
import { formatNumber } from "../../shared/format";
import { settingsApi } from "./api";
import type { SettingsApi } from "./api";

type SettingsField =
  | "login_autostart"
  | "system_notifications"
  | "notify_max_n"
  | "retention_days"
  | "language";

type FieldErrors = Partial<Record<SettingsField, string>>;

interface SettingsDraft {
  language: Locale;
  loginAutostart: boolean;
  notifyMaxN: string;
  notifyMode: "bounded" | "all";
  retentionDays: "30" | "90" | "365" | "unlimited";
  systemNotifications: boolean;
}

type LoadState =
  | { kind: "loading" }
  | { kind: "ready"; document: SettingsDocument }
  | { kind: "error"; error: DisplayError };

type SaveState =
  | { kind: "idle" }
  | { kind: "saving" }
  | { kind: "success" }
  | { kind: "error"; error: DisplayError; fieldErrors: FieldErrors };

export interface SettingsViewProps {
  api?: SettingsApi | undefined;
  locale: Locale;
  synchronization?: ResourceSynchronization<SettingsEvent> | undefined;
}

const settingsFields = new Set<SettingsField>([
  "login_autostart",
  "system_notifications",
  "notify_max_n",
  "retention_days",
  "language",
]);

function isSettingsField(value: string): value is SettingsField {
  return settingsFields.has(value as SettingsField);
}

function toDraft(settings: Settings): SettingsDraft {
  return {
    language: settings.language,
    loginAutostart: settings.login_autostart,
    notifyMaxN: String(settings.notify_max_n ?? 1),
    notifyMode: settings.notify_max_n === null ? "all" : "bounded",
    retentionDays:
      settings.retention_days === null
        ? "unlimited"
        : String(settings.retention_days),
    systemNotifications: settings.system_notifications,
  } as SettingsDraft;
}

function parseDraft(
  draft: SettingsDraft,
):
  | { ok: true; settings: Settings; fieldErrors: FieldErrors }
  | { ok: false; fieldErrors: FieldErrors } {
  let notifyMaxN: number | null = null;
  if (draft.notifyMode === "bounded") {
    if (draft.notifyMaxN.trim().length === 0) {
      return { ok: false, fieldErrors: { notify_max_n: "required" } };
    }
    if (!/^\d+$/.test(draft.notifyMaxN.trim())) {
      return { ok: false, fieldErrors: { notify_max_n: "invalid_format" } };
    }
    notifyMaxN = Number(draft.notifyMaxN);
    if (
      !Number.isSafeInteger(notifyMaxN) ||
      notifyMaxN < 1 ||
      notifyMaxN > 1000
    ) {
      return { ok: false, fieldErrors: { notify_max_n: "range" } };
    }
  }

  const retentionDays =
    draft.retentionDays === "unlimited"
      ? null
      : (Number(draft.retentionDays) as 30 | 90 | 365);

  return {
    ok: true,
    settings: {
      language: draft.language,
      login_autostart: draft.loginAutostart,
      notify_max_n: notifyMaxN,
      retention_days: retentionDays,
      system_notifications: draft.systemNotifications,
    },
    fieldErrors: {},
  };
}

function draftsEqual(left: SettingsDraft, right: SettingsDraft): boolean {
  return (
    left.language === right.language &&
    left.loginAutostart === right.loginAutostart &&
    left.notifyMaxN === right.notifyMaxN &&
    left.notifyMode === right.notifyMode &&
    left.retentionDays === right.retentionDays &&
    left.systemNotifications === right.systemNotifications
  );
}

function isRevisionError(error: DisplayError): boolean {
  return (
    error.code === "revision_conflict" || error.code === "precondition_required"
  );
}

function isOlderOrEqualRevision(incoming: string, current: string): boolean {
  if (incoming === current) {
    return true;
  }
  if (/^\d+$/.test(incoming) && /^\d+$/.test(current)) {
    return BigInt(incoming) <= BigInt(current);
  }
  return false;
}

export function SettingsView({
  api = settingsApi,
  locale,
  synchronization,
}: SettingsViewProps) {
  const { t } = useTranslation("settings");
  const { t: tCommon } = useTranslation("common");
  const { t: tErrors } = useTranslation("errors");
  const [loadAttempt, setLoadAttempt] = useState(0);
  const [loadState, setLoadState] = useState<LoadState>({ kind: "loading" });
  const [draft, setDraft] = useState<SettingsDraft | null>(null);
  const [saveState, setSaveState] = useState<SaveState>({ kind: "idle" });
  const currentDocumentRef = useRef<SettingsDocument | null>(null);
  const currentDraftRef = useRef<SettingsDraft | null>(null);
  const startedSyncEpochRef = useRef<number | null>(null);
  const processedDeliveryRef = useRef(0);
  const syncEpoch = synchronization?.epoch;
  const synchronized = synchronization !== undefined;
  const onSnapshotComplete = synchronization?.onSnapshotComplete;
  const deliveredEvents = synchronization?.events;

  useEffect(() => {
    if (synchronized && syncEpoch === null) {
      return;
    }
    const startsNewSync =
      typeof syncEpoch === "number" &&
      startedSyncEpochRef.current !== syncEpoch;
    const existingDocument = currentDocumentRef.current;
    const existingDraft = currentDraftRef.current;
    const preserveDraft =
      startsNewSync &&
      existingDocument !== null &&
      existingDraft !== null &&
      !draftsEqual(existingDraft, toDraft(existingDocument.settings));
    if (startsNewSync) {
      startedSyncEpochRef.current = syncEpoch;
      setLoadState({ kind: "loading" });
    }
    let active = true;
    void api.get().then(
      (document) => {
        if (active) {
          void dashboardI18n.changeLanguage(document.settings.language);
          currentDocumentRef.current = document;
          const draftMatchesDocument =
            existingDraft !== null &&
            draftsEqual(existingDraft, toDraft(document.settings));
          if (!preserveDraft || draftMatchesDocument) {
            const nextDraft = toDraft(document.settings);
            currentDraftRef.current = nextDraft;
            setDraft(nextDraft);
            setSaveState({ kind: "idle" });
          } else if (existingDocument.revision !== document.revision) {
            setSaveState({
              kind: "error",
              error: { code: "revision_conflict", params: {} },
              fieldErrors: {},
            });
          } else {
            setSaveState({ kind: "idle" });
          }
          setLoadState({ kind: "ready", document });
          if (typeof syncEpoch === "number") {
            onSnapshotComplete?.("settings", syncEpoch);
          }
        }
      },
      (error: unknown) => {
        if (active) {
          setLoadState({ kind: "error", error: toDisplayError(error) });
        }
      },
    );
    return () => {
      active = false;
    };
  }, [api, loadAttempt, onSnapshotComplete, syncEpoch, synchronized]);

  useEffect(() => {
    const pending = (deliveredEvents ?? []).filter(
      (delivery) => delivery.deliveryId > processedDeliveryRef.current,
    );
    if (pending.length === 0) {
      return;
    }
    processedDeliveryRef.current = pending.at(-1)?.deliveryId ?? 0;
    if (saveState.kind === "saving") {
      return;
    }

    for (const delivery of pending) {
      const incoming = delivery.event.document;
      const currentDocument = currentDocumentRef.current;
      if (
        currentDocument !== null &&
        isOlderOrEqualRevision(incoming.revision, currentDocument.revision)
      ) {
        continue;
      }
      const existingDraft = currentDraftRef.current;
      const wasDirty =
        currentDocument !== null &&
        existingDraft !== null &&
        !draftsEqual(existingDraft, toDraft(currentDocument.settings));
      const hasUnsavedDraft =
        wasDirty && !draftsEqual(existingDraft, toDraft(incoming.settings));
      currentDocumentRef.current = incoming;
      setLoadState({ kind: "ready", document: incoming });
      void dashboardI18n.changeLanguage(incoming.settings.language);
      if (hasUnsavedDraft) {
        setSaveState({
          kind: "error",
          error: { code: "revision_conflict", params: {} },
          fieldErrors: {},
        });
        continue;
      }

      const nextDraft = toDraft(incoming.settings);
      currentDraftRef.current = nextDraft;
      setDraft(nextDraft);
      setSaveState({ kind: "idle" });
    }
  }, [deliveredEvents, saveState.kind]);

  const parsedDraft = useMemo(
    () => (draft === null ? null : parseDraft(draft)),
    [draft],
  );
  const dirty =
    loadState.kind === "ready" &&
    draft !== null &&
    !draftsEqual(draft, toDraft(loadState.document.settings));
  const busy = saveState.kind === "saving";
  const revisionBlocked =
    saveState.kind === "error" && isRevisionError(saveState.error);

  const updateDraft = (next: Partial<SettingsDraft>) => {
    setDraft((current) => {
      const updated = current === null ? null : { ...current, ...next };
      currentDraftRef.current = updated;
      return updated;
    });
    setSaveState({ kind: "idle" });
  };

  const reload = () => {
    setLoadState({ kind: "loading" });
    setSaveState({ kind: "idle" });
    setLoadAttempt((value) => value + 1);
  };

  const localValidationMessage = (code: string): string => {
    switch (code) {
      case "required":
        return t("validation.required");
      case "invalid_format":
        return t("validation.invalidFormat");
      case "range":
        return t("validation.range", {
          minimum: formatNumber(1, locale),
          maximum: formatNumber(1000, locale),
        });
      default:
        return t("validation.invalid");
    }
  };

  const issueMessage = (
    issue: NonNullable<DisplayError["params"]["issues"]>[number],
  ): string => {
    switch (issue.reason) {
      case "required":
        return t("validation.required");
      case "invalid_format":
        return t("validation.invalidFormat");
      case "unsupported":
        return t("validation.unsupported");
      case "inconsistent":
        return t("validation.inconsistent");
      case "out_of_range":
        if (issue.minimum !== undefined && issue.maximum !== undefined) {
          return t("validation.range", {
            minimum: formatNumber(issue.minimum, locale),
            maximum: formatNumber(issue.maximum, locale),
          });
        }
        if (issue.minimum !== undefined) {
          return t("validation.minimum", {
            minimum: formatNumber(issue.minimum, locale),
          });
        }
        if (issue.maximum !== undefined) {
          return t("validation.maximum", {
            maximum: formatNumber(issue.maximum, locale),
          });
        }
        return t("validation.outOfRange");
    }
  };

  const save = async (event: SyntheticEvent<HTMLFormElement, SubmitEvent>) => {
    event.preventDefault();
    if (
      loadState.kind !== "ready" ||
      draft === null ||
      parsedDraft === null ||
      revisionBlocked
    ) {
      return;
    }
    if (!parsedDraft.ok) {
      const fieldErrors = Object.fromEntries(
        Object.entries(parsedDraft.fieldErrors).map(([field, code]) => [
          field,
          localValidationMessage(code),
        ]),
      ) as FieldErrors;
      setSaveState({
        kind: "error",
        error: { code: "validation_failed", params: {} },
        fieldErrors,
      });
      return;
    }

    setSaveState({ kind: "saving" });
    try {
      const updated = await api.replace(
        parsedDraft.settings,
        loadState.document.revision,
      );
      await dashboardI18n.changeLanguage(updated.settings.language);
      const nextDraft = toDraft(updated.settings);
      currentDocumentRef.current = updated;
      currentDraftRef.current = nextDraft;
      setDraft(nextDraft);
      setLoadState({ kind: "ready", document: updated });
      setSaveState({ kind: "success" });
    } catch (error: unknown) {
      const displayError = toDisplayError(error);
      const fieldErrors: FieldErrors = {};
      if (displayError.code === "validation_failed") {
        for (const issue of displayError.params.issues ?? []) {
          if (
            isSettingsField(issue.field) &&
            fieldErrors[issue.field] === undefined
          ) {
            fieldErrors[issue.field] = issueMessage(issue);
          }
        }
      }
      setSaveState({ kind: "error", error: displayError, fieldErrors });
    }
  };

  const fieldError = (field: SettingsField): string | undefined =>
    saveState.kind === "error" ? saveState.fieldErrors[field] : undefined;

  return (
    <section
      className="feature-panel settings-panel"
      aria-labelledby="settings-heading"
    >
      <header className="feature-heading">
        <h2 id="settings-heading">{t("heading")}</h2>
      </header>

      {loadState.kind === "loading" ? (
        <div className="feature-state" role="status">
          {t("state.loading")}
        </div>
      ) : null}
      {loadState.kind === "error" ? (
        <div className="feature-state feature-state--error" role="alert">
          <strong>{t("state.errorTitle")}</strong>
          <span>{tErrors(errorTranslationKey(loadState.error.code))}</span>
          <button type="button" className="secondary-button" onClick={reload}>
            {tCommon("retry")}
          </button>
        </div>
      ) : null}

      {loadState.kind === "ready" && draft !== null ? (
        <form
          className="settings-form"
          aria-label={t("form.label")}
          onSubmit={(event) => {
            void save(event);
          }}
        >
          <div className="settings-grid">
            <label className="setting-card setting-card--toggle">
              <span className="setting-copy">
                <strong>{t("fields.autostart.label")}</strong>
                {fieldError("login_autostart") === undefined ? null : (
                  <span
                    id="login-autostart-error"
                    className="setting-error"
                    role="alert"
                  >
                    {fieldError("login_autostart")}
                  </span>
                )}
              </span>
              <input
                type="checkbox"
                checked={draft.loginAutostart}
                disabled={busy}
                aria-invalid={fieldError("login_autostart") !== undefined}
                aria-describedby={
                  fieldError("login_autostart") === undefined
                    ? undefined
                    : "login-autostart-error"
                }
                onChange={(event) => {
                  updateDraft({ loginAutostart: event.currentTarget.checked });
                }}
              />
            </label>

            <label className="setting-card setting-card--toggle">
              <span className="setting-copy">
                <strong>{t("fields.notifications.label")}</strong>
                {fieldError("system_notifications") === undefined ? null : (
                  <span
                    id="system-notifications-error"
                    className="setting-error"
                    role="alert"
                  >
                    {fieldError("system_notifications")}
                  </span>
                )}
              </span>
              <input
                type="checkbox"
                checked={draft.systemNotifications}
                disabled={busy}
                aria-invalid={fieldError("system_notifications") !== undefined}
                aria-describedby={
                  fieldError("system_notifications") === undefined
                    ? undefined
                    : "system-notifications-error"
                }
                onChange={(event) => {
                  updateDraft({
                    systemNotifications: event.currentTarget.checked,
                  });
                }}
              />
            </label>

            <fieldset className="setting-card setting-card--fieldset">
              <legend>{t("fields.threshold.label")}</legend>
              <div className="setting-input-row">
                <label>
                  <span>{t("fields.threshold.modeLabel")}</span>
                  <select
                    value={draft.notifyMode}
                    disabled={busy}
                    onChange={(event) => {
                      updateDraft({
                        notifyMode:
                          event.currentTarget.value === "all"
                            ? "all"
                            : "bounded",
                      });
                    }}
                  >
                    <option value="bounded">
                      {t("fields.threshold.modes.bounded")}
                    </option>
                    <option value="all">
                      {t("fields.threshold.modes.all")}
                    </option>
                  </select>
                </label>
                {draft.notifyMode === "bounded" ? (
                  <label>
                    <span>{t("fields.threshold.valueLabel")}</span>
                    <input
                      type="number"
                      min="1"
                      max="1000"
                      step="1"
                      inputMode="numeric"
                      value={draft.notifyMaxN}
                      disabled={busy}
                      aria-invalid={fieldError("notify_max_n") !== undefined}
                      aria-describedby={
                        fieldError("notify_max_n") === undefined
                          ? undefined
                          : "notify-max-n-error"
                      }
                      onChange={(event) => {
                        updateDraft({ notifyMaxN: event.currentTarget.value });
                      }}
                    />
                  </label>
                ) : null}
              </div>
              {fieldError("notify_max_n") === undefined ? null : (
                <span
                  id="notify-max-n-error"
                  className="setting-error"
                  role="alert"
                >
                  {fieldError("notify_max_n")}
                </span>
              )}
            </fieldset>

            <label className="setting-card setting-card--select">
              <span className="setting-copy">
                <strong>{t("fields.retention.label")}</strong>
              </span>
              <select
                value={draft.retentionDays}
                disabled={busy}
                aria-invalid={fieldError("retention_days") !== undefined}
                aria-describedby={
                  fieldError("retention_days") === undefined
                    ? undefined
                    : "retention-days-error"
                }
                onChange={(event) => {
                  updateDraft({
                    retentionDays: event.currentTarget
                      .value as SettingsDraft["retentionDays"],
                  });
                }}
              >
                <option value="30">
                  {t("fields.retention.options.days30")}
                </option>
                <option value="90">
                  {t("fields.retention.options.days90")}
                </option>
                <option value="365">
                  {t("fields.retention.options.days365")}
                </option>
                <option value="unlimited">
                  {t("fields.retention.options.unlimited")}
                </option>
              </select>
              {fieldError("retention_days") === undefined ? null : (
                <span
                  id="retention-days-error"
                  className="setting-error"
                  role="alert"
                >
                  {fieldError("retention_days")}
                </span>
              )}
            </label>

            <label className="setting-card setting-card--select">
              <span className="setting-copy">
                <strong>{t("fields.language.label")}</strong>
              </span>
              <select
                value={draft.language}
                disabled={busy}
                aria-invalid={fieldError("language") !== undefined}
                aria-describedby={
                  fieldError("language") === undefined
                    ? undefined
                    : "language-error"
                }
                onChange={(event) => {
                  updateDraft({
                    language:
                      event.currentTarget.value === "zh-CN" ? "zh-CN" : "en-US",
                  });
                }}
              >
                <option value="en-US">
                  {t("fields.language.options.enUS")}
                </option>
                <option value="zh-CN">
                  {t("fields.language.options.zhCN")}
                </option>
              </select>
              {fieldError("language") === undefined ? null : (
                <span
                  id="language-error"
                  className="setting-error"
                  role="alert"
                >
                  {fieldError("language")}
                </span>
              )}
            </label>
          </div>

          {saveState.kind === "success" ? (
            <p
              className="settings-message settings-message--success"
              role="status"
            >
              {t("messages.saved")}
            </p>
          ) : null}
          {saveState.kind === "error" ? (
            <div
              className="settings-message settings-message--error"
              role="alert"
            >
              <strong>
                {isRevisionError(saveState.error)
                  ? t("messages.conflictTitle")
                  : t("messages.saveErrorTitle")}
              </strong>
              <span>{tErrors(errorTranslationKey(saveState.error.code))}</span>
              {isRevisionError(saveState.error) ? (
                <button
                  type="button"
                  className="secondary-button"
                  onClick={reload}
                >
                  {t("actions.reload")}
                </button>
              ) : null}
            </div>
          ) : null}

          <div className="settings-actions">
            <button
              type="button"
              className="secondary-button"
              disabled={!dirty || busy}
              onClick={() => {
                const nextDraft = toDraft(loadState.document.settings);
                currentDraftRef.current = nextDraft;
                setDraft(nextDraft);
                setSaveState({ kind: "idle" });
              }}
            >
              {t("actions.reset")}
            </button>
            <button
              type="submit"
              className="primary-button"
              disabled={!dirty || busy || revisionBlocked}
            >
              {busy ? t("actions.saving") : t("actions.save")}
            </button>
          </div>
        </form>
      ) : null}
    </section>
  );
}
