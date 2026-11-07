import type { Page } from "@playwright/test";

export interface ContrastCheck {
  minimum: number;
  name: string;
  ratio: number;
}

export async function themeContrastChecks(
  page: Page,
): Promise<ContrastCheck[]> {
  return page.evaluate(() => {
    interface Color {
      alpha: number;
      blue: number;
      green: number;
      red: number;
    }

    const rootStyle = getComputedStyle(document.documentElement);
    const probe = document.createElement("span");
    probe.hidden = true;
    document.body.append(probe);

    const color = (variable: string): Color => {
      probe.style.color = rootStyle.getPropertyValue(variable).trim();
      const normalized = getComputedStyle(probe).color;
      const channels = normalized.match(/\d*\.?\d+/gu);
      if (channels === null || channels.length < 3) {
        throw new Error(`Could not parse ${variable}: ${normalized}`);
      }
      return {
        alpha: channels.length >= 4 ? Number(channels[3]) : 1,
        blue: Number(channels[2]) / 255,
        green: Number(channels[1]) / 255,
        red: Number(channels[0]) / 255,
      };
    };
    const composite = (foreground: Color, background: Color): Color => ({
      alpha: 1,
      blue:
        foreground.blue * foreground.alpha +
        background.blue * (1 - foreground.alpha),
      green:
        foreground.green * foreground.alpha +
        background.green * (1 - foreground.alpha),
      red:
        foreground.red * foreground.alpha +
        background.red * (1 - foreground.alpha),
    });
    const linearChannel = (value: number) =>
      value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
    const luminance = (value: Color) =>
      0.2126 * linearChannel(value.red) +
      0.7152 * linearChannel(value.green) +
      0.0722 * linearChannel(value.blue);
    const contrast = (left: Color, right: Color) => {
      const [lighter, darker] = [luminance(left), luminance(right)].sort(
        (first, second) => second - first,
      );
      return (lighter + 0.05) / (darker + 0.05);
    };

    const page = color("--page-background");
    const panel = color("--panel-background");
    const card = color("--card-background");
    const header = color("--header-background");
    const glow = composite(color("--page-glow"), page);
    const primary = color("--text-primary");
    const secondary = color("--text-secondary");
    const tertiary = color("--text-tertiary");
    const accent = color("--accent");
    const border = color("--border-strong");
    const pairs: {
      background: Color;
      foreground: Color;
      minimum: number;
      name: string;
    }[] = [
      {
        background: page,
        foreground: primary,
        minimum: 4.5,
        name: "primary text on page",
      },
      {
        background: glow,
        foreground: secondary,
        minimum: 4.5,
        name: "secondary text on page glow",
      },
      {
        background: panel,
        foreground: secondary,
        minimum: 4.5,
        name: "secondary text on panel",
      },
      {
        background: card,
        foreground: secondary,
        minimum: 4.5,
        name: "secondary text on card",
      },
      {
        background: glow,
        foreground: tertiary,
        minimum: 4.5,
        name: "tertiary text on page glow",
      },
      {
        background: panel,
        foreground: tertiary,
        minimum: 4.5,
        name: "tertiary text on panel",
      },
      {
        background: header,
        foreground: tertiary,
        minimum: 4.5,
        name: "tertiary text on table header",
      },
      {
        background: accent,
        foreground: page,
        minimum: 4.5,
        name: "primary button text",
      },
      {
        background: color("--accent-soft"),
        foreground: accent,
        minimum: 4.5,
        name: "accent status text",
      },
      {
        background: color("--error-soft"),
        foreground: color("--error"),
        minimum: 4.5,
        name: "error text",
      },
      {
        background: color("--warning-soft"),
        foreground: color("--warning"),
        minimum: 4.5,
        name: "warning text",
      },
      {
        background: page,
        foreground: border,
        minimum: 3,
        name: "control boundary on page",
      },
      {
        background: panel,
        foreground: border,
        minimum: 3,
        name: "control boundary on panel",
      },
      {
        background: card,
        foreground: border,
        minimum: 3,
        name: "control boundary on card",
      },
      {
        background: page,
        foreground: accent,
        minimum: 3,
        name: "focus indicator on page",
      },
      {
        background: panel,
        foreground: accent,
        minimum: 3,
        name: "focus indicator on panel",
      },
      {
        background: card,
        foreground: accent,
        minimum: 3,
        name: "focus indicator on card",
      },
    ];
    probe.remove();
    return pairs.map(({ background, foreground, minimum, name }) => ({
      minimum,
      name,
      ratio: contrast(foreground, background),
    }));
  });
}
