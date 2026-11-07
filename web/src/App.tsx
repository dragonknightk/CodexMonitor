import type { KeyboardEvent } from "react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { AlertsView } from "./features/alerts";
import type { AlertsApi } from "./features/alerts";
import { CallsView } from "./features/calls";
import type { CallsApi } from "./features/calls";
import { useDashboardSynchronization } from "./features/events";
import type { DashboardEventsApi } from "./features/events";
import {
  SourcesView,
  sourcesApi as productionSourcesApi,
} from "./features/sources";
import type { SourcesApi } from "./features/sources";
import { SettingsView } from "./features/settings";
import type { SettingsApi } from "./features/settings";
import { localeFromLanguage } from "./localization";

export interface AppProps {
  callsApi?: CallsApi | undefined;
  alertsApi?: AlertsApi | undefined;
  sourcesApi?: SourcesApi | undefined;
  settingsApi?: SettingsApi | undefined;
  eventsApi?: DashboardEventsApi | undefined;
}

const dashboardTabs = ["calls", "alerts", "sources", "settings"] as const;
type DashboardTab = (typeof dashboardTabs)[number];

export function App({
  callsApi,
  alertsApi,
  sourcesApi,
  settingsApi,
  eventsApi,
}: AppProps) {
  const { t: tCommon } = useTranslation("common");
  const { t: tDashboard, i18n } = useTranslation("dashboard");
  const locale = localeFromLanguage(i18n.resolvedLanguage);
  const synchronization = useDashboardSynchronization({ api: eventsApi });
  const resolvedSourcesApi = sourcesApi ?? productionSourcesApi;
  const [activeTab, setActiveTab] = useState<DashboardTab>("calls");
  const tabRefs = useRef<Record<DashboardTab, HTMLButtonElement | null>>({
    calls: null,
    alerts: null,
    sources: null,
    settings: null,
  });

  const handleTabKeyDown = (
    event: KeyboardEvent<HTMLButtonElement>,
    index: number,
  ) => {
    let nextIndex: number | null = null;
    if (event.key === "ArrowRight") {
      nextIndex = (index + 1) % dashboardTabs.length;
    } else if (event.key === "ArrowLeft") {
      nextIndex = (index - 1 + dashboardTabs.length) % dashboardTabs.length;
    } else if (event.key === "Home") {
      nextIndex = 0;
    } else if (event.key === "End") {
      nextIndex = dashboardTabs.length - 1;
    }
    if (nextIndex === null) {
      return;
    }
    event.preventDefault();
    const nextTab = dashboardTabs[nextIndex];
    if (nextTab !== undefined) {
      setActiveTab(nextTab);
      tabRefs.current[nextTab]?.focus();
    }
  };

  return (
    <div className="app-layout">
      <header className="topbar">
        <div className="topbar-inner">
          <div className="topbar-brand">
            <span className="topbar-dot" aria-hidden="true" />
            <h1>{tCommon("productName")}</h1>
          </div>
          <div
            className="tab-list"
            role="tablist"
            aria-label={tDashboard("nav.label")}
          >
            {dashboardTabs.map((tab, index) => {
              const selected = activeTab === tab;
              return (
                <button
                  key={tab}
                  ref={(node) => {
                    tabRefs.current[tab] = node;
                  }}
                  id={`tab-${tab}`}
                  type="button"
                  role="tab"
                  className={`tab-button${selected ? " tab-button--active" : ""}`}
                  aria-selected={selected}
                  aria-controls={`panel-${tab}`}
                  tabIndex={selected ? 0 : -1}
                  onClick={() => {
                    setActiveTab(tab);
                  }}
                  onKeyDown={(event) => {
                    handleTabKeyDown(event, index);
                  }}
                >
                  {tDashboard(`nav.${tab}`)}
                </button>
              );
            })}
          </div>
        </div>
      </header>

      <main className="app-shell">
        <div
          id="panel-calls"
          role="tabpanel"
          aria-labelledby="tab-calls"
          className="tab-panel"
          hidden={activeTab !== "calls"}
        >
          <CallsView
            api={callsApi}
            synchronization={{
              epoch: synchronization.epoch,
              events: synchronization.modelCall,
              onSnapshotComplete: synchronization.reportSnapshot,
            }}
          />
        </div>

        <div
          id="panel-alerts"
          role="tabpanel"
          aria-labelledby="tab-alerts"
          className="tab-panel"
          hidden={activeTab !== "alerts"}
        >
          <AlertsView
            api={alertsApi}
            locale={locale}
            synchronization={{
              epoch: synchronization.epoch,
              events: synchronization.alert,
              onSnapshotComplete: synchronization.reportSnapshot,
            }}
          />
        </div>

        <div
          id="panel-sources"
          role="tabpanel"
          aria-labelledby="tab-sources"
          className="tab-panel"
          hidden={activeTab !== "sources"}
        >
          <SourcesView
            api={resolvedSourcesApi}
            locale={locale}
            synchronization={{
              epoch: synchronization.epoch,
              events: synchronization.sourceHealth,
              onSnapshotComplete: synchronization.reportSnapshot,
            }}
          />
        </div>

        <div
          id="panel-settings"
          role="tabpanel"
          aria-labelledby="tab-settings"
          className="tab-panel"
          hidden={activeTab !== "settings"}
        >
          <SettingsView
            api={settingsApi}
            locale={locale}
            synchronization={{
              epoch: synchronization.epoch,
              events: synchronization.settings,
              onSnapshotComplete: synchronization.reportSnapshot,
            }}
          />
        </div>
      </main>
    </div>
  );
}
