import * as Sentry from "@sentry/nextjs";
import { getServerSentryOptions } from "@/lib/sentry-config";

const options = getServerSentryOptions(process.env);

if (options) {
  Sentry.init(options);
}
