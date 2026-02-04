import { hc } from "hono/client";
import type { AppType } from "../../../dev-cloud/src/index.ts";

export const api = hc<AppType>(window.location.origin);
