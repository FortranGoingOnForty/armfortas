! Regression (fpm path dependencies unresolvable): a use-imported
! character-returning FUNCTION must not bind to an earlier same-named function
! in an unrelated module (fpm: M_CLI2::join_path vs fpm_filesystem::join_path).
module m_cli
  implicit none
  private
  public :: jp
contains
  function jp(a1, a2, a3) result(path)
     character(len=*), intent(in) :: a1, a2
     character(len=*), intent(in), optional :: a3
     character(len=:), allocatable :: path
     path = 'CLI['//trim(a1)//'+'//trim(a2)//']'
     if (present(a3)) path = path//'+'//trim(a3)
  end function
end module

module m_fs
  implicit none
  private
  public :: jp
contains
  function jp(a1, a2, a3) result(path)
     character(len=*), intent(in) :: a1, a2
     character(len=*), intent(in), optional :: a3
     character(len=:), allocatable :: path
     if (a1 == "") then
        path = a2
     else
        path = a1 // '/' // a2
     end if
     if (present(a3)) path = path // '/' // a3
  end function
end module

module m_dep
  use m_fs, only : jp
  implicit none
  private
  public :: resolve
contains
  subroutine resolve(root, dpath)
     character(len=*), intent(in) :: root, dpath
     character(len=:), allocatable :: proj, manifest
     proj = jp(root, dpath)
     manifest = jp(proj, 'fpm.toml')
     write(*,'(a)') 'manifest='//manifest
  end subroutine
end module

program p
  use m_dep, only : resolve
  implicit none
  call resolve('.', '../toml-f')
end program
! CHECK: manifest=./../toml-f/fpm.toml
