//! Projective coordinates for a point on a Short Weierstrass curve
//! ([Homogeneous coordinates]).
//!
//! [Homogeneous coordinates]: https://en.wikipedia.org/wiki/Homogeneous_coordinates

use alloc::vec::Vec;
use core::{
    borrow::Borrow,
    fmt::{Debug, Display, Formatter},
    hash::{Hash, Hasher},
    ops::{Add, AddAssign, Mul, MulAssign, Neg, Sub, SubAssign},
};

use educe::Educe;
use num_traits::{One, Zero};
use zeroize::Zeroize;

use super::{Affine, SWCurveConfig};
use crate::{
    bits::BitIteratorBE,
    curve::{batch_inversion, AffineRepr, CurveGroup, PrimeGroup},
    field::{group::AdditiveGroup, prime::PrimeField, Field},
    impl_additive_ops_from_ref,
};

/// Jacobian coordinates for a point on an elliptic curve in short Weierstrass
/// form, over the base field `P::BaseField`. This struct implements arithmetic
/// via the Jacobian formulae.
#[derive(Educe)]
#[educe(Copy, Clone)]
#[must_use]
pub struct Projective<P: SWCurveConfig> {
    /// `X / Z` projection of the affine `X`
    pub x: P::BaseField,
    /// `Y / Z` projection of the affine `Y`
    pub y: P::BaseField,
    /// Projective multiplicative inverse. Will be `0` only at infinity.
    pub z: P::BaseField,
}

impl<P: SWCurveConfig> Display for Projective<P> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", Affine::from(*self))
    }
}

impl<P: SWCurveConfig> Debug for Projective<P> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        if self.is_zero() {
            write!(f, "infinity")
        } else {
            write!(f, "({}, {}, {})", self.x, self.y, self.z)
        }
    }
}

impl<P: SWCurveConfig> Eq for Projective<P> {}
impl<P: SWCurveConfig> PartialEq for Projective<P> {
    fn eq(&self, other: &Self) -> bool {
        if self.is_zero() {
            return other.is_zero();
        }

        if other.is_zero() {
            return false;
        }

        // The points (X, Y, Z) and (X', Y', Z')
        // are equal when (X * Z'^2) = (X' * Z^2)
        // and (Y * Z'^3) = (Y' * Z^3).
        let z1z1 = self.z.square();
        let z2z2 = other.z.square();

        if self.x * z2z2 == other.x * z1z1 {
            self.y * (z2z2 * other.z) == other.y * (z1z1 * self.z)
        } else {
            false
        }
    }
}

impl<P: SWCurveConfig> PartialEq<Affine<P>> for Projective<P> {
    fn eq(&self, other: &Affine<P>) -> bool {
        self == &other.into_group()
    }
}

impl<P: SWCurveConfig> Hash for Projective<P> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.into_affine().hash(state);
    }
}

impl<P: SWCurveConfig> Default for Projective<P> {
    #[inline]
    fn default() -> Self {
        Self::zero()
    }
}

impl<P: SWCurveConfig> Projective<P> {
    /// Constructs a new group element without checking whether the coordinates
    /// specify a point in the subgroup.
    pub const fn new_unchecked(
        x: P::BaseField,
        y: P::BaseField,
        z: P::BaseField,
    ) -> Self {
        Self { x, y, z }
    }

    /// Constructs a new group element in a way while enforcing that points are
    /// in the prime-order subgroup.
    ///
    /// # Panics
    ///
    /// * If point is not on curve.
    /// * If point is not in the prime-order subgroup.
    pub fn new(x: P::BaseField, y: P::BaseField, z: P::BaseField) -> Self {
        let p = Self::new_unchecked(x, y, z).into_affine();
        assert!(p.is_on_curve());
        assert!(p.is_in_prime_order_subgroup());
        p.into()
    }
}

impl<P: SWCurveConfig> Zeroize for Projective<P> {
    fn zeroize(&mut self) {
        self.x.zeroize();
        self.y.zeroize();
        self.z.zeroize();
    }
}

impl<P: SWCurveConfig> Zero for Projective<P> {
    /// Returns the point at infinity, which always has Z = 0.
    #[inline]
    fn zero() -> Self {
        Self::new_unchecked(
            P::BaseField::one(),
            P::BaseField::one(),
            P::BaseField::zero(),
        )
    }

    /// Checks whether `self.z.is_zero()`.
    #[inline]
    fn is_zero(&self) -> bool {
        self.z == P::BaseField::ZERO
    }
}

impl<P: SWCurveConfig> AdditiveGroup for Projective<P> {
    type Scalar = P::ScalarField;

    const ZERO: Self = Self::new_unchecked(
        P::BaseField::ONE,
        P::BaseField::ONE,
        P::BaseField::ZERO,
    );

    /// Sets `self = 2 * self`. Note that Jacobian formulae are incomplete, and
    /// so doubling cannot be computed as `self + self`. Instead, this
    /// implementation uses the following specialized doubling formulae:
    ///
    /// * [`P::A` is zero](http://www.hyperelliptic.org/EFD/g1p/auto-shortw-jacobian-0.html#doubling-dbl-2009-l)
    /// * [`P::A` is not zero](https://www.hyperelliptic.org/EFD/g1p/auto-shortw-jacobian.html#doubling-dbl-2007-bl)
    fn double_in_place(&mut self) -> &mut Self {
        if self.is_zero() {
            return self;
        }

        if P::COEFF_A == P::BaseField::ZERO {
            // A = X1^2
            let mut a = self.x;
            a.square_in_place();

            // B = Y1^2
            let mut b = self.y;
            b.square_in_place();

            // C = B^2
            let mut c = b;
            c.square_in_place();

            // D = 2*((X1+B)^2-A-C)
            //   = 2 * (X1 + Y1^2)^2 - A - C
            //   = 2 * 2 * X1 * Y1^2
            let d = if [1, 2].contains(&P::BaseField::extension_degree()) {
                let mut d = self.x;
                d *= &b;
                d.double_in_place().double_in_place();
                d
            } else {
                let mut d = self.x;
                d += &b;
                d.square_in_place();
                d -= a;
                d -= c;
                d.double_in_place();
                d
            };

            // E = 3*A
            let e = a + a.double_in_place();

            // Z3 = 2*Y1*Z1
            self.z *= &self.y;
            self.z.double_in_place();

            // F = E^2
            // X3 = F-2*D
            self.x = e;
            self.x.square_in_place();
            self.x -= &d.double();

            // Y3 = E*(D-X3)-8*C
            self.y = d;
            self.y -= &self.x;
            self.y *= &e;
            self.y -= c.double_in_place().double_in_place().double_in_place();
            self
        } else {
            // http://www.hyperelliptic.org/EFD/g1p/auto-shortw-jacobian-0.html#doubling-dbl-2009-l
            // XX = X1^2
            let xx = self.x.square();

            // YY = Y1^2
            let yy = self.y.square();

            // YYYY = YY^2
            let mut yyyy = yy;
            yyyy.square_in_place();

            // ZZ = Z1^2
            let mut zz = self.z;
            zz.square_in_place();

            // S = 2*((X1+YY)^2-XX-YYYY)
            let s = ((self.x + yy).square() - xx - yyyy).double();

            // M = 3*XX+a*ZZ^2
            let mut m = xx;
            m.double_in_place();
            m += &xx;
            m += &P::mul_by_a(zz.square());

            // T = M^2-2*S
            // X3 = T
            self.x = m;
            self.x.square_in_place();
            self.x -= s.double();

            // Z3 = (Y1+Z1)^2-YY-ZZ
            // Can be calculated as Z3 = 2*Y1*Z1, and this is faster.
            self.z *= self.y;
            self.z.double_in_place();

            // Y3 = M*(S-X3)-8*YYYY
            self.y = s;
            self.y -= &self.x;
            self.y *= &m;
            self.y -=
                yyyy.double_in_place().double_in_place().double_in_place();

            self
        }
    }
}

impl<P: SWCurveConfig> PrimeGroup for Projective<P> {
    type ScalarField = P::ScalarField;

    #[inline]
    fn generator() -> Self {
        Affine::generator().into()
    }

    #[inline]
    fn mul_bigint(&self, other: impl BitIteratorBE) -> Self {
        P::mul_projective(self, other)
    }
}

impl<P: SWCurveConfig> CurveGroup for Projective<P> {
    type Affine = Affine<P>;
    type BaseField = P::BaseField;
    type Config = P;
    type FullGroup = Affine<P>;

    /// Normalizes a slice of projective elements so that
    /// conversion to affine is inexpensive.
    ///
    /// In more detail, this method converts a curve point in Jacobian
    /// coordinates (x, y, z) into an equivalent representation (x/z^2,
    /// y/z^3, 1).
    ///
    /// For `N = v.len()`, this costs 1 inversion + 6N field multiplications + N
    /// field squarings.
    ///
    /// (Where batch inversion comprises 3N field multiplications + 1 inversion
    /// of these operations)
    #[inline]
    fn normalize_batch(v: &[Self]) -> Vec<Self::Affine> {
        let mut z_s = v.iter().map(|g| g.z).collect::<Vec<_>>();

        batch_inversion(&mut z_s);

        // Perform affine transformations.
        v.iter()
            .zip(z_s)
            .map(|(g, z)| {
                if g.is_zero() {
                    Affine::identity()
                } else {
                    let z2 = z.square();
                    let x = g.x * z2;
                    let y = g.y * z2 * z;
                    Affine::new_unchecked(x, y)
                }
            })
            .collect()
    }
}

impl<P: SWCurveConfig> Neg for Projective<P> {
    type Output = Self;

    #[inline]
    fn neg(mut self) -> Self {
        self.y = -self.y;
        self
    }
}

impl<P: SWCurveConfig, T: Borrow<Affine<P>>> AddAssign<T> for Projective<P> {
    /// Using <http://www.hyperelliptic.org/EFD/g1p/auto-shortw-jacobian-0.html#addition-madd-2007-bl>
    fn add_assign(&mut self, other: T) {
        let other = other.borrow();
        if let Some((other_x, other_y)) = other.xy() {
            if self.is_zero() {
                self.x = other_x;
                self.y = other_y;
                self.z = P::BaseField::one();
                return;
            }

            // Z1Z1 = Z1^2
            let mut z1z1 = self.z;
            z1z1.square_in_place();

            // U2 = X2*Z1Z1
            let mut u2 = other_x;
            u2 *= &z1z1;

            // S2 = Y2*Z1*Z1Z1
            let mut s2 = self.z;
            s2 *= &other_y;
            s2 *= &z1z1;

            if self.x == u2 {
                if self.y == s2 {
                    // The two points are equal, so we double.
                    self.double_in_place();
                } else {
                    // a + (-a) = 0
                    *self = Self::zero();
                }
            } else {
                // H = U2-X1
                let mut h = u2;
                h -= &self.x;

                // HH = H^2
                let mut hh = h;
                hh.square_in_place();

                // I = 4*HH
                let mut i = hh;
                i.double_in_place().double_in_place();

                // J = -H*I
                let mut j = h;
                j.neg_in_place();
                j *= &i;

                // r = 2*(S2-Y1)
                let mut r = s2;
                r -= &self.y;
                r.double_in_place();

                // V = X1*I
                let mut v = self.x;
                v *= &i;

                // X3 = r^2 + J - 2*V
                self.x = r.square();
                self.x += &j;
                self.x -= &v.double();

                // Y3 = r*(V-X3) + 2*Y1*J
                v -= &self.x;
                self.y.double_in_place();
                self.y = P::BaseField::sum_of_products(&[r, self.y], &[v, j]);

                // Z3 = 2 * Z1 * H;
                // Can alternatively be computed as (Z1+H)^2-Z1Z1-HH, but the
                // latter is slower.
                self.z *= &h;
                self.z.double_in_place();
            }
        }
    }
}

impl<P: SWCurveConfig, T: Borrow<Affine<P>>> Add<T> for Projective<P> {
    type Output = Self;

    fn add(mut self, other: T) -> Self {
        let other = other.borrow();
        self += other;
        self
    }
}

impl<P: SWCurveConfig, T: Borrow<Affine<P>>> SubAssign<T> for Projective<P> {
    fn sub_assign(&mut self, other: T) {
        *self += -(*other.borrow());
    }
}

impl<P: SWCurveConfig, T: Borrow<Affine<P>>> Sub<T> for Projective<P> {
    type Output = Self;

    fn sub(mut self, other: T) -> Self {
        self -= other.borrow();
        self
    }
}

impl_additive_ops_from_ref!(Projective, SWCurveConfig);

impl<'a, P: SWCurveConfig> Add<&'a Self> for Projective<P> {
    type Output = Self;

    #[inline]
    fn add(mut self, other: &'a Self) -> Self {
        self += other;
        self
    }
}

impl<'a, P: SWCurveConfig> AddAssign<&'a Self> for Projective<P> {
    fn add_assign(&mut self, other: &'a Self) {
        if self.is_zero() {
            *self = *other;
            return;
        }

        if other.is_zero() {
            return;
        }

        // http://www.hyperelliptic.org/EFD/g1p/auto-shortw-jacobian-0.html#addition-add-2007-bl
        // Works for all curves.

        // Z1Z1 = Z1^2
        let z1z1 = self.z.square();

        // Z2Z2 = Z2^2
        let z2z2 = other.z.square();

        // U1 = X1*Z2Z2
        let mut u1 = self.x;
        u1 *= &z2z2;

        // U2 = X2*Z1Z1
        let mut u2 = other.x;
        u2 *= &z1z1;

        // S1 = Y1*Z2*Z2Z2
        let mut s1 = self.y;
        s1 *= &other.z;
        s1 *= &z2z2;

        // S2 = Y2*Z1*Z1Z1
        let mut s2 = other.y;
        s2 *= &self.z;
        s2 *= &z1z1;

        if u1 == u2 {
            if s1 == s2 {
                // The two points are equal, so we double.
                self.double_in_place();
            } else {
                // a + (-a) = 0
                *self = Self::zero();
            }
        } else {
            // H = U2-U1
            let mut h = u2;
            h -= &u1;

            // I = (2*H)^2
            let mut i = h;
            i.double_in_place().square_in_place();

            // J = -H*I
            let mut j = h;
            j.neg_in_place();
            j *= &i;

            // r = 2*(S2-S1)
            let mut r = s2;
            r -= &s1;
            r.double_in_place();

            // V = U1*I
            let mut v = u1;
            v *= &i;

            // X3 = r^2 + J - 2*V
            self.x = r;
            self.x.square_in_place();
            self.x += &j;
            self.x -= &(v.double());

            // Y3 = r*(V - X3) + 2*S1*J
            v -= &self.x;
            self.y = s1;
            self.y.double_in_place();
            self.y = P::BaseField::sum_of_products(&[r, self.y], &[v, j]);

            // Z3 = ((Z1+Z2)^2 - Z1Z1 - Z2Z2)*H
            // This is equal to Z3 = 2 * Z1 * Z2 * H, and computing it this way
            // is faster.
            self.z *= other.z;
            self.z.double_in_place();
            self.z *= &h;
        }
    }
}

impl<'a, P: SWCurveConfig> Sub<&'a Self> for Projective<P> {
    type Output = Self;

    #[inline]
    fn sub(mut self, other: &'a Self) -> Self {
        self -= other;
        self
    }
}

impl<'a, P: SWCurveConfig> SubAssign<&'a Self> for Projective<P> {
    fn sub_assign(&mut self, other: &'a Self) {
        *self += &(-(*other));
    }
}

impl<P: SWCurveConfig, T: Borrow<P::ScalarField>> MulAssign<T>
    for Projective<P>
{
    fn mul_assign(&mut self, other: T) {
        *self = self.mul_bigint(other.borrow().into_bigint());
    }
}

impl<P: SWCurveConfig, T: Borrow<P::ScalarField>> Mul<T> for Projective<P> {
    type Output = Self;

    #[inline]
    fn mul(mut self, other: T) -> Self {
        self *= other;
        self
    }
}

// The affine point X, Y is represented in the Jacobian
// coordinates with Z = 1.
impl<P: SWCurveConfig> From<Affine<P>> for Projective<P> {
    #[inline]
    fn from(p: Affine<P>) -> Projective<P> {
        p.xy().map_or(Projective::zero(), |(x, y)| Self {
            x,
            y,
            z: P::BaseField::one(),
        })
    }
}

impl<P: SWCurveConfig, T: Borrow<Affine<P>>> core::iter::Sum<T>
    for Projective<P>
{
    fn sum<I: Iterator<Item = T>>(iter: I) -> Self {
        iter.fold(Projective::zero(), |sum, x| sum + x.borrow())
    }
}
