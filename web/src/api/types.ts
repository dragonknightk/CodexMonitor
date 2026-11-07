import type { components, operations } from "./schema";

export type ApiErrorCode = components["schemas"]["ApiErrorCode"];
export type ApiErrorResponse = components["schemas"]["ErrorResponse"];
export type ErrorParameters = ApiErrorResponse["error"]["params"];
export type CurrentSession = components["schemas"]["CurrentSession"];
export type HealthSnapshot = components["schemas"]["HealthSnapshot"];
export type Capabilities = components["schemas"]["Capabilities"];
export type ModelCall = components["schemas"]["ModelCall"];
export type ModelCallPage = components["schemas"]["ModelCallPage"];
export type ModelCallSummary = components["schemas"]["ModelCallSummary"];
export type Conversation = components["schemas"]["Conversation"];
export type ConversationPage = components["schemas"]["ConversationPage"];
export type SourceHealth = components["schemas"]["SourceHealth"];
export type SourceList = components["schemas"]["SourceList"];
export type Alert = components["schemas"]["Alert"];
export type AlertPage = components["schemas"]["AlertPage"];
export type Settings = components["schemas"]["Settings"];
export type SettingsDocument = components["schemas"]["SettingsDocument"];
export type Locale = components["schemas"]["Locale"];
export type ModelCallEvent = components["schemas"]["ModelCallEvent"];
export type SourceHealthEvent = components["schemas"]["SourceHealthEvent"];
export type AlertEvent = components["schemas"]["AlertEvent"];
export type SettingsEvent = components["schemas"]["SettingsEvent"];
export type StreamControlEvent = components["schemas"]["StreamControlEvent"];
export type PageMetadata = components["schemas"]["PageMetadata"];

export type ModelCallQuery = NonNullable<
  operations["listModelCalls"]["parameters"]["query"]
>;
export type SummaryQuery = NonNullable<
  operations["getModelCallSummary"]["parameters"]["query"]
>;
export type ConversationQuery = NonNullable<
  operations["listConversations"]["parameters"]["query"]
>;
export type AlertQuery = NonNullable<
  operations["listAlerts"]["parameters"]["query"]
>;
