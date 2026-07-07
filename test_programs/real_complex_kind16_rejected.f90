! Regression (audit C7): real(16)/complex(16) (IEEE quad) must be rejected,
! not silently downgraded to single precision. The IR maps real kind 8 -> f64
! and every other kind -> f32 (src/ir/types.rs float_from_kind), so a real(16)
! variable reported kind(x)==4 and was computed in single precision; a
! complex(16) additionally mis-sized its buffer and SIGSEGV'd at exit. The
! backend has no float wider than 64 bits, so sema now rejects any real/complex
! kind outside {4, 8} loudly. Supported kinds keep working — see
! real_kind_supported_ok.f90.
!
! Residual: a bare `1.0_16` literal with no quad variable to hold it still
! reports kind 4 (the literal-kind path has no validation hook yet).
!
! ERROR_EXPECTED: is not supported
program real_complex_kind16_rejected
  real(16) :: x
  complex(16) :: z
  x = 1.0
  z = (1.0, 2.0)
  print *, x, z
end program
