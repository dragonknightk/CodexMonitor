import { dashboardApi, requestPage } from "../../api/client";
import type { ConversationPage, ConversationQuery } from "../../api/types";

export interface ConversationsApi {
  list(query: ConversationQuery): Promise<ConversationPage>;
}

export const conversationsApi: ConversationsApi = {
  list(query) {
    return requestPage(
      dashboardApi.GET("/api/v1/conversations", { params: { query } }),
    );
  },
};
