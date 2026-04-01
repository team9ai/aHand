import { render, screen } from "@testing-library/react";
import DashboardHomePage from "@/app/(dashboard)/page";

describe("dashboard home page", () => {
  it("renders the authenticated landing page content", () => {
    render(<DashboardHomePage />);

    expect(screen.getByRole("heading", { name: /dashboard ready/i })).toBeInTheDocument();
    expect(screen.getByText(/signed in through the shared hub auth shell/i)).toBeInTheDocument();
  });
});
