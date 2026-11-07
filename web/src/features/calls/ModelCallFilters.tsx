import type { SubmitEvent } from "react";
import { useTranslation } from "react-i18next";

import type { Locale } from "../../api/types";
import { formatNumber } from "../../shared/format";
import {
  alertFilterOptions,
  modelCallSortOptions,
  recentCountOptions,
} from "./query";
import type {
  AlertFilter,
  ModelCallFilterDraft,
  ModelCallSort,
  RecentCount,
} from "./query";

export interface ModelCallFiltersProps {
  draft: ModelCallFilterDraft;
  locale: Locale;
  validationError: string | null;
  busy: boolean;
  onChange: (draft: ModelCallFilterDraft) => void;
  onApply: () => void;
  onReset: () => void;
}

function isRecentCount(value: number): value is RecentCount {
  return recentCountOptions.some((option) => option === value);
}

function isModelCallSort(value: string): value is ModelCallSort {
  return modelCallSortOptions.some((option) => option === value);
}

function isAlertFilter(value: string): value is AlertFilter {
  return alertFilterOptions.some((option) => option === value);
}

export function ModelCallFilters({
  draft,
  locale,
  validationError,
  busy,
  onChange,
  onApply,
  onReset,
}: ModelCallFiltersProps) {
  const { t } = useTranslation("dashboard");

  const submit = (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault();
    onApply();
  };

  return (
    <form
      className="filter-panel"
      aria-label={t("calls.filters.label")}
      onSubmit={submit}
    >
      <fieldset className="filter-fieldset">
        <legend className="sr-only">{t("calls.filters.label")}</legend>
        <div className="filter-grid">
          <label className="filter-control">
            <span>{t("calls.filters.recentCount")}</span>
            <select
              value={draft.recentCount}
              onChange={(event) => {
                const value = Number(event.currentTarget.value);
                if (isRecentCount(value)) {
                  onChange({ ...draft, recentCount: value });
                }
              }}
            >
              {recentCountOptions.map((count) => (
                <option key={count} value={count}>
                  {formatNumber(count, locale)}
                </option>
              ))}
            </select>
          </label>

          <label className="filter-control filter-control--wide">
            <span>{t("calls.filters.conversation")}</span>
            <input
              type="search"
              value={draft.conversation}
              maxLength={256}
              placeholder={t("calls.filters.conversationPlaceholder")}
              autoComplete="off"
              onChange={(event) => {
                onChange({ ...draft, conversation: event.currentTarget.value });
              }}
            />
          </label>

          <label className="filter-control">
            <span>{t("calls.filters.model")}</span>
            <input
              type="search"
              value={draft.model}
              maxLength={128}
              placeholder={t("calls.filters.modelPlaceholder")}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => {
                onChange({ ...draft, model: event.currentTarget.value });
              }}
            />
          </label>

          <label className="filter-control">
            <span>{t("calls.filters.source")}</span>
            <input
              type="search"
              value={draft.sourceId}
              maxLength={64}
              placeholder={t("calls.filters.sourcePlaceholder")}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => {
                onChange({ ...draft, sourceId: event.currentTarget.value });
              }}
            />
          </label>

          <label className="filter-control">
            <span>{t("calls.filters.from")}</span>
            <input
              type="datetime-local"
              value={draft.from}
              aria-invalid={validationError !== null}
              aria-describedby={
                validationError === null
                  ? undefined
                  : "model-call-time-range-error"
              }
              onChange={(event) => {
                onChange({ ...draft, from: event.currentTarget.value });
              }}
            />
          </label>

          <label className="filter-control">
            <span>{t("calls.filters.to")}</span>
            <input
              type="datetime-local"
              value={draft.to}
              aria-invalid={validationError !== null}
              aria-describedby={
                validationError === null
                  ? undefined
                  : "model-call-time-range-error"
              }
              onChange={(event) => {
                onChange({ ...draft, to: event.currentTarget.value });
              }}
            />
          </label>

          <label className="filter-control">
            <span>{t("calls.filters.alertStatus")}</span>
            <select
              value={draft.alert}
              onChange={(event) => {
                const value = event.currentTarget.value;
                if (isAlertFilter(value)) {
                  onChange({ ...draft, alert: value });
                }
              }}
            >
              {alertFilterOptions.map((option) => (
                <option key={option} value={option}>
                  {t(`calls.filters.alertOptions.${option}`)}
                </option>
              ))}
            </select>
          </label>

          <label className="filter-control">
            <span>{t("calls.filters.sort")}</span>
            <select
              value={draft.sort}
              onChange={(event) => {
                const value = event.currentTarget.value;
                if (isModelCallSort(value)) {
                  onChange({ ...draft, sort: value });
                }
              }}
            >
              {modelCallSortOptions.map((option) => (
                <option key={option} value={option}>
                  {t(`calls.filters.sortOptions.${option}`)}
                </option>
              ))}
            </select>
          </label>
        </div>

        <div className="filter-footer">
          <div className="filter-actions">
            <button type="submit" className="primary-button" disabled={busy}>
              {t("calls.filters.apply")}
            </button>
            <button
              type="button"
              className="secondary-button"
              disabled={busy}
              onClick={onReset}
            >
              {t("calls.filters.clear")}
            </button>
          </div>
          {validationError === null ? null : (
            <p
              id="model-call-time-range-error"
              className="filter-error"
              role="alert"
            >
              {validationError}
            </p>
          )}
        </div>
      </fieldset>
    </form>
  );
}
