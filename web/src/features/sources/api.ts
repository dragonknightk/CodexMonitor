import {
  CSRF_HEADER_NAME,
  dashboardApi,
  getCsrfProof,
  requestJson,
} from "../../api/client";
import type { SourceHealth, SourceList } from "../../api/types";

export interface SourcesApi {
  list(): Promise<SourceList>;
  retry(sourceId: SourceHealth["source_id"]): Promise<SourceHealth>;
}

export const sourcesApi: SourcesApi = {
  list() {
    return requestJson(dashboardApi.GET("/api/v1/sources"));
  },
  async retry(sourceId) {
    const proof = await getCsrfProof();
    return requestJson(
      dashboardApi.POST("/api/v1/sources/{source_id}/retry", {
        params: {
          header: { [CSRF_HEADER_NAME]: proof },
          path: { source_id: sourceId },
        },
      }),
    );
  },
};
