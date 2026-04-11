! fpm-inspired source suffix classifier shape.
! Conservatively written because the typed constructor spelling
! `[character(len=20) :: ...]` is still a separate parser-gap note.
! XFAIL: audit FPM-SUFFIX-1 (fpm-style suffix scan hits i32/i64 IR store mismatch)
! CHECK: 3
! CHECK: 2
! CHECK: 2
! CHECK: 322
! IR_CHECK: call @classify_sources(
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module suffix_scan_mod
  implicit none
contains
  pure logical function has_suffix(name, suffix)
    implicit none
    character(len=*), intent(in) :: name
    character(len=*), intent(in) :: suffix
    integer(8) :: pos

    pos = index(name, suffix)
    has_suffix = pos > 0_8
  end function has_suffix

  subroutine classify_sources(names, nf90, nfixed, ncpp)
    implicit none
    character(len=*), intent(in) :: names(:)
    integer, intent(out) :: nf90, nfixed, ncpp
    integer :: i

    nf90 = 0
    nfixed = 0
    ncpp = 0

    do i = 1, size(names)
      if (has_suffix(names(i), ".f90")) then
        nf90 = nf90 + 1
      else if (has_suffix(names(i), ".f")) then
        nfixed = nfixed + 1
      else if (has_suffix(names(i), ".F90")) then
        ncpp = ncpp + 1
      end if
    end do
  end subroutine classify_sources
end module suffix_scan_mod

program realworld_suffix_scan
  use suffix_scan_mod
  implicit none
  character(len=20) :: names(8)
  integer :: nf90, nfixed, ncpp

  names(1) = "main.f90"
  names(2) = "lexer.f90"
  names(3) = "legacy.f"
  names(4) = "build.F90"
  names(5) = "notes.txt"
  names(6) = "scan.f90"
  names(7) = "pack.f"
  names(8) = "driver.F90"

  call classify_sources(names, nf90, nfixed, ncpp)
  print *, nf90, nfixed, ncpp, 100 * nf90 + 10 * nfixed + ncpp
end program realworld_suffix_scan
