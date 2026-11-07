import { appendFileSync, readFileSync } from "node:fs";
import process from "node:process";

const eventPath = process.env.GITHUB_EVENT_PATH;
const outputPath = process.env.GITHUB_OUTPUT;

if (!eventPath || !outputPath) {
  throw new Error("GITHUB_EVENT_PATH and GITHUB_OUTPUT are required");
}

const event = JSON.parse(readFileSync(eventPath, "utf8"));
const body =
  typeof event.pull_request?.body === "string" ? event.pull_request.body : "";
const impact =
  /^\s*docs-impact:\s*(required|optional|none)\s*$/imu.exec(body)?.[1] ?? "";
const reason = /^\s*docs-impact-reason:\s*(.*?)\s*$/imu.exec(body)?.[1] ?? "";

appendFileSync(
  outputPath,
  `docs_impact=${impact.replaceAll("\n", " ")}\n`,
  "utf8",
);
appendFileSync(
  outputPath,
  `docs_impact_reason=${reason.replaceAll("\n", " ")}\n`,
  "utf8",
);
