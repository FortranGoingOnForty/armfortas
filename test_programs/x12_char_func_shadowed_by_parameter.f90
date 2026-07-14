! A character-returning function must keep its hidden-result (sret) ABI
! at call sites even when an unrelated scope declares a PARAMETER of the
! same name. Here module `charconst_mod` has `character(26) :: lower` and
! module `strings_mod` has a `lower` function; the callee lowers `lower`
! with a hidden string-descriptor result buffer, but the call site's ABI
! lookup resolved the name through a global any-scope search that
! short-circuited on the parameter, dropping the ABI. The caller then
! emitted a scalar-byte call against the sret callee ("store: value type
! i8 doesn't match pointee type ptr<i8>") and IR verification rejected
! the module. Surfaced building fpm: `fpm_toml::name_is_json` calls
! `str_ends_with(lower(filename), ...)` while `M_CLI2` defines
! `character(26), parameter :: lower`. x12.
!
! CHECK: json=T
! CHECK: notjson=F
! CHECK: plen=26
module charconst_mod
  implicit none
  character(len=26), parameter :: lower = 'abcdefghijklmnopqrstuvwxyz'
end module

module strings_mod
  implicit none
  interface str_ends_with
     procedure :: str_ends_with_str
  end interface
contains
  elemental pure function lower(str) result(string)
    character(*), intent(in) :: str
    character(len(str)) :: string
    integer :: i
    string = str
    do i = 1, len_trim(str)
      select case (str(i:i))
      case ('A':'Z'); string(i:i) = char(iachar(str(i:i)) + 32)
      case default
      end select
    end do
  end function

  pure logical function str_ends_with_str(s, e) result(r)
    character(*), intent(in) :: s, e
    integer :: n1, n2
    n1 = len(s) - len(e) + 1
    n2 = len(s)
    if (n1 < 1) then
      r = .false.
    else
      r = (s(n1:n2) == e)
    end if
  end function
end module

module toml_mod
  use strings_mod, only: lower, str_ends_with
  implicit none
contains
  logical function name_is_json(filename)
    character(*), intent(in) :: filename
    character(*), parameter :: json_identifier = ".json"
    name_is_json = .false.
    if (len_trim(filename) < len(json_identifier)) return
    name_is_json = str_ends_with(lower(filename), json_identifier)
  end function
end module

program p
  use toml_mod, only: name_is_json
  use charconst_mod, only: lower
  implicit none
  write(*, '(a,l1)') 'json=', name_is_json('a.JSON')
  write(*, '(a,l1)') 'notjson=', name_is_json('a.txt')
  write(*, '(a,i0)') 'plen=', len(lower)
end program p
