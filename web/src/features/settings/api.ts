import {
  CSRF_HEADER_NAME,
  dashboardApi,
  getCsrfProof,
  requestJson,
} from "../../api/client";
import type { Settings, SettingsDocument } from "../../api/types";

export interface SettingsApi {
  get(): Promise<SettingsDocument>;
  replace(
    settings: Settings,
    revision: SettingsDocument["revision"],
  ): Promise<SettingsDocument>;
}

export const settingsApi: SettingsApi = {
  get() {
    return requestJson(dashboardApi.GET("/api/v1/settings"));
  },
  async replace(settings, revision) {
    const proof = await getCsrfProof();
    return requestJson(
      dashboardApi.PUT("/api/v1/settings", {
        body: settings,
        params: {
          header: {
            [CSRF_HEADER_NAME]: proof,
            "If-Match": `"${revision}"`,
          },
        },
      }),
    );
  },
};
