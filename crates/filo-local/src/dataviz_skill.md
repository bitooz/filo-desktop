---
name: dataviz
description: "Read before making any chart, graph, plot, dashboard, stat tile or other data visualization, in any medium (an HTML artifact, SVG, matplotlib, plotly, d3, Recharts, a PNG): picking the chart, its colours, labels and layout."
---

# Charts that read at a glance

Work through these in order before writing chart code.

## 1. Say what the chart is for

Write one sentence: what should the reader see? ("Sales doubled after March", "Rent is the biggest cost.")
Pick the form that shows that one thing; if no sentence comes, a table may be better than a chart.

## 2. Pick the form

| The point | Use | Avoid |
|---|---|---|
| Change over time | Line (many points), columns (few periods) | Pie, area stacks with more than 3 series |
| Compare amounts | Horizontal bars, sorted, starting at zero | 3D, radial bars |
| Part of a whole | One stacked bar, or a pie with at most 4 slices | Pies with many slices, donuts inside donuts |
| Distribution | Histogram, box or dot plot | Averages alone |
| Relationship of two measures | Scatter plot | Two y-axes |
| A single number | A stat tile: the value, a label, the change against a reference | A gauge |
| Ranking | Sorted bars or a dot plot | Unsorted bars |

Bars always start at zero. Lines may start elsewhere when the change is the point; say so on the axis.

## 3. Colour

- One colour for one series. Grey for context, one accent for what the sentence is about.
- Categories: at most 6 distinct hues, then group the rest as "Other". Keep a category's colour the same in every chart.
- Ordered values (low to high): one hue from light to dark. Values around a midpoint (loss/gain): two hues meeting at a neutral grey.
- Default categorical palette (works on light and dark): `#3B82F6` blue, `#F59E0B` amber, `#10B981` green, `#EF4444` red, `#8B5CF6` violet, `#64748B` slate.
- Never tell series apart by colour alone: add direct labels, markers or dashes. Check text contrast is at least 4.5:1.
- Light and dark: use the page's text colour for labels and a faint version of it (about 15% opacity) for grid lines.

## 4. Marks and labels

- Label lines and bars directly at their end instead of a legend when there are up to 5 series.
- Title states the finding ("Rent is 41% of spending"), a subtitle gives units and period.
- Few, light grid lines; no chart borders, shadows, gradients or 3D.
- Round numbers to what matters (12.4k, not 12,384.27). Units on the axis or in the subtitle, once.
- Dates: short months ("Mar"), the year only where it changes.
- Sort categories by value unless they have a natural order (days, sizes).

## 5. Layout of several charts

- A row of stat tiles first, then the main chart, then detail.
- Charts compared side by side share the same scale and colours.
- Align to a grid; same padding everywhere; text at most two sizes.

## 6. Interaction (HTML)

- Tooltips show the exact value, its unit and the date or category.
- Hover highlights the series under the pointer and fades the others.
- Everything readable without hovering: tooltips add detail, they don't carry the point.

## 7. Check before sharing

- Does the title say the one thing from step 1?
- Can someone colour-blind tell every series apart?
- Does it read in both light and dark backgrounds, and at half size?
- Are the numbers right? Spot-check two values against the data.
