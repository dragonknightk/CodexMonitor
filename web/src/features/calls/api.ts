import { dashboardApi, requestJson, requestPage } from "../../api/client";
import type {
  ModelCallPage,
  ModelCallQuery,
  ModelCallSummary,
  SummaryQuery,
} from "../../api/types";

export interface CallsApi {
  list(query: ModelCallQuery): Promise<ModelCallPage>;
  summarize(query: SummaryQuery): Promise<ModelCallSummary>;
}

export const callsApi: CallsApi = {
  list(query) {
    return requestPage(
      dashboardApi.GET("/api/v1/model-calls", { params: { query } }),
    );
  },
  summarize(query) {
    return requestJson(
      dashboardApi.GET("/api/v1/summaries", { params: { query } }),
    );
  },
};
