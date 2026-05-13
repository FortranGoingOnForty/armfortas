! Stdlib drill: nmhash32x's submodule body selects the low length lane via
! a parent-module logical PARAMETER imported through .amod.
! MULTIFILE_LINK: nmhash_mod.f90 nmhash_impl.f90 main.f90
! CHECK: ok
! REPRO_CHECK: run
!--- file: nmhash_mod.f90
module nmhash_mod
  use iso_fortran_env, only: int8, int16, int32
  implicit none

  private
  public :: int8_nmhash32x
  logical, parameter :: little_endian = (1 == transfer([1_int8, 0_int8], 0_int16))

  interface
    pure module function int8_nmhash32x(key, seed) result(hash)
      integer(int32) :: hash
      integer(int8), intent(in) :: key(0:)
      integer(int32), intent(in) :: seed
    end function int8_nmhash32x
  end interface
end module nmhash_mod
!--- file: nmhash_impl.f90
submodule(nmhash_mod) nmhash_impl
  use iso_fortran_env, only: int8, int32, int64
  implicit none

contains
  pure function readle32(p) result(v)
    integer(int32) :: v
    integer(int8), intent(in) :: p(:)

    v = transfer(p(1:4), 0_int32)
  end function readle32

  pure module function int8_nmhash32x(key, seed) result(hash)
    integer(int32) :: hash
    integer(int8), intent(in) :: key(0:)
    integer(int32), intent(in) :: seed
    integer(int64) :: len

    len = size(key, kind=int64)
    if (len < 256) then
      hash = nmhash32x_9to255(key, seed)
      return
    end if
    hash = 0
  end function int8_nmhash32x

  pure function nmhash32x_9to255(p, seed) result(x)
    integer(int8), intent(in) :: p(0:)
    integer(int32), intent(in) :: seed
    integer(int32) :: x
    integer(int64) :: len
    integer(int32) :: len32(0:1), len_base
    integer(int32) :: y
    integer(int32) :: a, b
    integer(int64) :: i, r

    len = size(p, kind=int64)
    len32 = transfer(len, 0_int32, 2)
    if (little_endian) then
      len_base = len32(0)
    else
      len_base = len32(1)
    end if
    x = int(z'C2B2AE3D', int32)
    y = seed
    a = int(z'27D4EB2F', int32)
    b = seed
    r = (len - 1)/16

    do i=0, r-1
      x = ieor(x, readle32(p(i*16 + 0:)))
      y = ieor(y, readle32(p(i*16 + 4:)))
      x = ieor(x, y)
      x = x * int(z'11049A7D', int32)
      x = ieor(x, ishft(x, -23))
      x = x * int(z'BCCCDC7B', int32)
      y = ishftc(y, 4)
      x = ieor(x, y)
      x = ieor(x, ishft(x, -12))
      x = x * int(z'065E9DAD', int32)
      x = ieor(x, ishft(x, -12))

      a = ieor(a, readle32(p(i*16 + 8:)))
      b = ieor(b, readle32(p(i*16 + 12:)))
      a = ieor(a, b)
      a = a * int(z'11049A7D', int32)
      a = ieor(a, ishft(a, -23))
      a = a * int(z'BCCCDC7B', int32)
      b = ishftc(b, 3)
      a = ieor(a, b)
      a = ieor(a, ishft(a, -12))
      a = a * int(z'065E9DAD', int32)
      a = ieor(a, ishft(a, -12))
    end do

    if (iand(len_base-1_int32, 8_int32) /= 0) then
      if (iand(len_base-1_int32, 4_int32) /= 0) then
        a = ieor(a, readle32(p(r*16 + 0:)))
        b = ieor(b, readle32(p(r*16 + 4:)))
        a = ieor(a, b)
        a = a * int(z'11049A7D', int32)
        a = ieor(a, ishft(a, -23))
        a = a * int(z'BCCCDC7B', int32)
        a = ieor(a, ishftc(b, 4))
        a = ieor(a, ishft(a, -12))
        a = a * int(z'065E9DAD', int32)
      else
        a = ieor(a, readle32(p(r*16:)) + b)
        a = ieor(a, ishft(a, -16))
        a = a * int(z'A52FB2CD', int32)
        a = ieor(a, ishft(a, -15))
        a = a * int(z'551E4D49', int32)
      end if
      x = ieor(x, readle32(p(len - 8:)))
      y = ieor(y, readle32(p(len - 4:)))
      x = ieor(x, y)
      x = x * int(z'11049A7D', int32)
      x = ieor(x, ishft(x, -23))
      x = x * int(z'BCCCDC7B', int32)
      x = ieor(x, ishftc(y, 3))
      x = ieor(x, ishft(x, -12))
      x = x * int(z'065E9DAD', int32)
    else
      if (iand(len_base-1_int32, 4_int32) /= 0) then
        a = ieor(a, readle32(p(r*16:)) + b)
        a = ieor(a, ishft(a, -16))
        a = a * int(z'A52FB2CD', int32)
        a = ieor(a, ishft(a, -15))
        a = a * int(z'551E4D49', int32)
      end if
      x = ieor(x, readle32(p(len - 4:)) + y)
      x = ieor(x, ishft(x, -16))
      x = x * int(z'A52FB2CD', int32)
      x = ieor(x, ishft(x, -15))
      x = x * int(z'551E4D49', int32)
    end if

    x = ieor(x, len_base)
    x = ieor(x, ishftc(a, 27))
    x = ieor(x, ishft(x, -14))
    x = x * int(z'141CC535', int32)
  end function nmhash32x_9to255
end submodule nmhash_impl
!--- file: main.f90
program p
  use iso_fortran_env, only: int8, int32
  use nmhash_mod, only: int8_nmhash32x
  implicit none

  integer(int8) :: key(300)
  integer :: i

  do i = 1, 300
    key(i) = int(iand(i, 255), int8)
  end do

  if (int8_nmhash32x(key(1:9), int(z'deadbeef', int32)) /= int(z'1A06128A', int32)) error stop 9
  if (int8_nmhash32x(key(1:32), int(z'deadbeef', int32)) /= int(z'EABBF1B8', int32)) error stop 32
  print *, 'ok'
end program p
