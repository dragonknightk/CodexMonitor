import { dashboardApi, requestPage } from "../../api/client";
import type { AlertPage, AlertQuery } from "../../api/types";

export interface AlertsApi {
  list(query: AlertQuery): Promise<AlertPage>;
}

export const alertsApi: AlertsApi = {
  list(query) {
    return requestPage(
      dashboardApi.GET("/api/v1/alerts", { params: { query } }),
    );
  },
};
