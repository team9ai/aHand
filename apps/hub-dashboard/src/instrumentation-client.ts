import * as Sentry from "@sentry/nextjs";
import { getBrowserSentryOptions } from "@/lib/sentry-config";

const options = getBrowserSentryOptions(process.env);

if (options) {
  Sentry.init(options);
}

export const onRouterTransitionStart = Sentry.captureRouterTransitionStart;
