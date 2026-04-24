# Math

## Inline

Pythagoras: $a^2 + b^2 = c^2$. The area of a circle is $\pi r^2$ and its circumference $2 \pi r$.

The relation $e^{i\pi} + 1 = 0$ ties together five of the most important constants.

Greek, operators, and relations: $\alpha + \beta \le \gamma$, $\sum = \int$, $x \in \mathbb{R}$, $x \to \infty$, $\Delta x \approx \epsilon$.

A fraction inline: $\frac{1}{2} + \frac{1}{3} = \frac{5}{6}$. A square root inline: $\sqrt{2} \approx 1.414$.

## Display

Quadratic formula:

$$ x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a} $$

Sum of squares:

$$ \sum_{k=1}^{n} k^2 = \frac{n(n+1)(2n+1)}{6} $$

Gaussian:

$$ \int_{-\infty}^{\infty} e^{-x^2} dx = \sqrt{\pi} $$

Nested fractions:

$$ \frac{1 + \frac{1}{x}}{1 - \frac{1}{x}} $$

## Accents

Hats and friends for forecast symbols and vectors:

$\hat{x}$, $\hat{\pi}$, $\hat{\ell}$, $\bar{y}$, $\tilde{z}$, $\vec{v}$, $\dot{q}$, $\ddot{r}$.

$$\hat{\pi}^{cap,up}_h \cdot \frac{cap_{up,h}}{10^6} + \hat{f}^{act,dn}_h \cdot \hat{q}^{ps}_h$$

## Named operators

Upright roman for the classics:

$\max(a, b)$, $\min\{0, x\}$, $\log n$, $\ln e = 1$, $\sin \theta$, $\cos 2\pi$, $\lim_{n \to \infty} \frac{1}{n} = 0$, $\exp(-x^2)$.

## Text in math

$$\text{throughput}_h = cap_{up,h} \cdot \hat{f}^{act,up}_h + cap_{dn,h} \cdot \hat{f}^{act,dn}_h$$

Also `\mathrm`: $\mathrm{Var}(X) = \mathbb{E}[X^2] - (\mathbb{E} X)^2$.

## Blackboard bold

Sets and the indicator: $\mathbb{R}$, $\mathbb{N}$, $\mathbb{Z}$, $\mathbb{C}$, $\mathbb{Q}$, $\mathbb{1}$, $x \in \mathbb{R}^n$.

$$\mathbb{1}[\hat{\ell}^{p90}_h > \hat{P}^{target}_{m,s}]$$

## Sized delimiters

Auto-sized `\left...\right`:

$$\hat{q}^{ps}_h = \max\left(0, \hat{\ell}^{p90}_h - \theta_h\right)$$

Fixed-size `\Big[ … \Big]`:

$$Z_s = \sum_{h=0}^{H-1} \Big[ R^{cap}_h + R^{en}_h + V^{peak}_h - D_h \Big] - T_s$$

Combined with a tall fraction (content drives the height):

$$\left( \frac{\hat{\ell}^{p90}_h}{E^{nom}_s} + \frac{cap_{up,h}}{P^{disc}_s} \right)$$

## Spacing and quantifiers

`\quad` / `\qquad` and `\forall` for constraints:

$$cap_{up,h} + r_h \le P^{disc}_s \qquad \forall h$$

$$0.05 \le soc_h \le 0.95 \quad \forall h$$

## Unknown commands

Fall back gracefully: $\heartsuit$ renders as `\heartsuit`.
