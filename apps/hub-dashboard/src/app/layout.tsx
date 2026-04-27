import "./globals.css";
import type { Metadata } from "next";
import type { ReactNode } from "react";
import { Providers } from "@/components/providers";

export const metadata: Metadata = {
  title: "aHand Hub Dashboard",
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en">
      <body className="hub-root">
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}
